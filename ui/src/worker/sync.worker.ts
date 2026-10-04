// The sync worker (DESIGN §2, §11.2): jess-core (WASM) + Yjs + IndexedDB + WebSocket.
// Everything that isn't rendering happens here.

/// <reference lib="webworker" />
import init, { Core, extract, Sha256, blobInfo as wasmBlobInfo } from '../wasm/core.js'
import * as Y from 'yjs'
import { openDb, loadAll, commit, scanPrefix, metaGet, putChunk, getChunk, deleteChunks, journalAppend, journalAll, journalDelete, type JournalEntry, type Write } from './idb'
import { reconnectDelay } from './backoff'
import type { Req, WorkerEvent, InitResult } from './protocol'
import type { EntryMeta, Extracted, SyncStatus } from '../lib/types'
import { SearchIndex, targetKey } from './search-index'
import { randomHex } from '../lib/ids'

declare const self: DedicatedWorkerGlobalScope

const VERSION = '0.1.0'
const CHUNK = 4 << 20

let db: IDBDatabase
let core: Core
let token: string | null = null
let base = ''
let ws: WebSocket | null = null
let attempt = 0
let wsFailures = 0
let httpMode = false
let foreground = true
let reconnectTimer: ReturnType<typeof setTimeout> | undefined
let queue: Promise<unknown> = Promise.resolve()
/** Doc updates received from the UI and not yet committed (status says "syncing" meanwhile). */
let unsaved = 0
/** Doc updates received from the UI, ever (reported in the status). */
let received = 0
const openDocs = new Map<string, number>()
// Each update is journalled (`keys`) as it arrives, then committed coalesced: a crash in between
// loses nothing, the journal is replayed at start-up (DESIGN §22 item 61).
const coalesce = new Map<string, { ups: Uint8Array[]; keys: Promise<number>[]; timer: ReturnType<typeof setTimeout> }>()
let index: SearchIndex | null = null
let indexing: Promise<void> | null = null
const dirtyDocs = new Set<string>()
let indexTimer: ReturnType<typeof setTimeout> | undefined
let blobPumping = false
let fatal: string | null = null

const post = (e: WorkerEvent, transfer: Transferable[] = []) => self.postMessage(e, transfer)

interface Output {
  writes: [Uint8Array, Uint8Array | null][]
  send: Uint8Array[]
  events: ({ t: 'entries'; ids: string[] } | { t: 'doc'; entry: string; slot: string; update: Uint8Array } | { t: 'rejected'; opId: number; reason: string } | { t: 'purged'; id: string } | { t: 'dead' } | { t: 'fatal'; message: string } | { t: 'status' })[]
}

/** Serialises every core call with its IndexedDB commit; frames go out only after the commit. */
function run<T extends Output | void>(f: () => T): Promise<void> {
  const p = queue.then(async () => {
    const o = f()
    if (o) await apply(o)
  })
  queue = p.catch((e) => console.error('jess worker:', e))
  return p
}

async function apply(o: Output) {
  await commit(db, o.writes as Write[])
  const pendingHttp: Uint8Array[] = []
  for (const frame of o.send) {
    if (ws && ws.readyState === WebSocket.OPEN) ws.send(frame)
    else if (httpMode) pendingHttp.push(frame)
  }
  if (pendingHttp.length) httpOutbox.push(...pendingHttp)
  let entryIds: string[] = []
  let statusChanged = false
  for (const e of o.events) {
    switch (e.t) {
      case 'entries':
        entryIds = entryIds.concat(e.ids)
        break
      case 'doc':
        if (e.slot === 'body') {
          if (openDocs.has(e.entry)) post({ ev: 'doc', id: e.entry, update: e.update })
          markDirty(e.entry)
        }
        break
      case 'rejected':
        post({ ev: 'rejected', opId: e.opId, reason: e.reason })
        break
      case 'purged':
        index?.remove(e.id)
        break
      case 'dead':
        ws?.close()
        break
      case 'fatal':
        fatal = e.message
        post({ ev: 'fatal', message: e.message })
        break
      case 'status':
        statusChanged = true
        break
    }
  }
  if (entryIds.length) {
    const list = JSON.parse(core.entriesJson([...new Set(entryIds)])) as (EntryMeta | { id: string; deleted: true })[]
    post({ ev: 'entries', list })
    for (const e of list) if ('kind' in e && e.kind === 'markdown') markDirty(e.id)
    if (index && list.some((e) => !('kind' in e) || e.kind === 'pdf')) schedulePdfIndex()
    if (list.some((e) => 'blob' in e && e.blob)) schedulePolicy()
  }
  if (statusChanged) postStatus()
  schedulePump()
}

function status(): SyncStatus {
  const s = JSON.parse(core.statusJson()) as SyncStatus
  if (fatal) return { ...s, state: 'error', error: fatal }
  if (!token) return { ...s, state: 'offline' }
  // Keystrokes still being coalesced or committed aren't in the core's outbox yet: not synced.
  if (unsaved > 0 && s.state === 'synced') return { ...s, state: 'syncing', docUpdates: received }
  return { ...s, docUpdates: received }
}

function postStatus() {
  post({ ev: 'status', status: status() })
}

// ---------------------------------------------------------------- transport

function wsUrl() {
  const u = new URL('/api/sync', base || self.location.origin)
  u.protocol = u.protocol === 'https:' ? 'wss:' : 'ws:'
  return u.toString()
}

function connect() {
  if (!token || ws || fatal) return
  clearTimeout(reconnectTimer)
  if (httpMode) {
    void httpLoop()
    return
  }
  let opened = false
  const sock = new WebSocket(wsUrl())
  sock.binaryType = 'arraybuffer'
  ws = sock
  sock.onopen = () => {
    opened = true
    wsFailures = 0
    void run(() => core.connected(token!, VERSION) as Output)
  }
  sock.onmessage = (m) => {
    const frame = new Uint8Array(m.data as ArrayBuffer)
    void run(() => {
      const o = core.frame(frame, Date.now()) as Output
      if (o.events.some((e) => e.t === 'status')) attempt = 0
      return o
    })
  }
  sock.onclose = () => {
    if (ws !== sock) return
    ws = null
    if (!opened && ++wsFailures >= 3) {
      httpMode = true // proxies that break WebSocket upgrades (DESIGN §5.3)
    }
    void run(() => core.disconnected() as Output)
    scheduleReconnect()
  }
}

function scheduleReconnect() {
  if (!token || fatal) return
  clearTimeout(reconnectTimer)
  reconnectTimer = setTimeout(connect, reconnectDelay(attempt++, foreground))
}

const httpOutbox: Uint8Array[] = []
let httpRunning = false

async function httpLoop() {
  if (httpRunning || !token) return
  httpRunning = true
  try {
    await run(() => {
      const o = core.connected(token!, VERSION) as Output
      httpOutbox.push(...o.send)
      return { ...o, send: [] }
    })
    while (httpMode && token && !fatal) {
      const frames = httpOutbox.splice(0)
      const body = core.httpRequest(frames, frames.some((f) => f.length > 0) && frames.length > 1 ? 0 : 20)
      let resp: Response
      try {
        resp = await fetch(new URL('/api/sync', base || self.location.origin), { method: 'POST', body: body as BodyInit, headers: { 'content-type': 'application/cbor' } })
      } catch {
        httpOutbox.unshift(...frames)
        await new Promise((r) => setTimeout(r, reconnectDelay(attempt++, foreground)))
        continue
      }
      if (!resp.ok) {
        httpOutbox.unshift(...frames)
        await new Promise((r) => setTimeout(r, reconnectDelay(attempt++, foreground)))
        continue
      }
      attempt = 0
      const bytes = new Uint8Array(await resp.arrayBuffer())
      await run(() => {
        const o = core.httpResponse(bytes, Date.now()) as Output
        // Frames produced while applying (pushes) are sent with the next request.
        httpOutbox.push(...o.send.filter((f) => f.length))
        const hello = core.connected(token!, VERSION) as Output
        httpOutbox.unshift(...hello.send)
        return { writes: o.writes.concat(hello.writes), send: [], events: o.events }
      })
      // Try WebSocket again occasionally.
      if (Math.random() < 0.05) {
        httpMode = false
        wsFailures = 0
        break
      }
    }
  } finally {
    httpRunning = false
    if (!httpMode) connect()
  }
}

setInterval(() => {
  if (!core || !ws) return
  void run(() => core.tick(Date.now()) as Output)
}, 1000)

function probe() {
  if (!core) return
  if (ws && ws.readyState === WebSocket.OPEN) {
    const ping = core.probe(Date.now())
    ws.send(ping)
    setTimeout(() => void run(() => core.tick(Date.now() + 3000) as Output), 2100)
  } else {
    // A socket stuck connecting (e.g. opened while the network was down) is replaced now.
    dropSocket()
    attempt = 0
    connect()
  }
}

function dropSocket() {
  const s = ws
  if (!s) return
  ws = null
  s.onclose = null
  s.onmessage = null
  s.close()
  void run(() => core.disconnected() as Output)
}

// ---------------------------------------------------------------- auth'd fetch

function authHeaders(extra: Record<string, string> = {}): HeadersInit {
  return { authorization: `Bearer ${token}`, ...extra }
}

const api = (p: string) => new URL(p, base || self.location.origin)

// ---------------------------------------------------------------- blobs

function schedulePump() {
  if (blobPumping || !token) return
  blobPumping = true
  queueMicrotask(() => void pumpBlobs().finally(() => (blobPumping = false)))
}

// The last few 4 MiB chunk records read, so PDF.js's many small range requests don't re-read
// the same IndexedDB record each time.
const chunkCache = new Map<string, Uint8Array>()
async function cachedChunk(hash: string, idx: number): Promise<Uint8Array | null> {
  const k = `${hash}:${idx}`
  const hit = chunkCache.get(k)
  if (hit) {
    chunkCache.delete(k)
    chunkCache.set(k, hit)
    return hit
  }
  const c = await getChunk(db, hash, idx)
  if (c) {
    chunkCache.set(k, c)
    while (chunkCache.size > 4) chunkCache.delete(chunkCache.keys().next().value!)
  }
  return c
}

async function readLocal(hash: string, offset: number, len: number): Promise<Uint8Array> {
  const out = new Uint8Array(len)
  let got = 0
  while (got < len) {
    const pos = offset + got
    const idx = Math.floor(pos / CHUNK)
    const c = await cachedChunk(hash, idx)
    if (!c) throw new Error('local blob chunk missing')
    const start = pos - idx * CHUNK
    const n = Math.min(len - got, c.length - start)
    out.set(c.subarray(start, start + n), got)
    got += n
  }
  return out
}

async function sha256hex(b: Uint8Array): Promise<string> {
  const d = await crypto.subtle.digest('SHA-256', b as BufferSource)
  return Array.from(new Uint8Array(d), (x) => x.toString(16).padStart(2, '0')).join('')
}

async function blobTask(t: { t: string; hash: string; size?: number; uploadId?: string; index?: number; offset?: number; len?: number; hashes?: string[] }): Promise<unknown> {
  try {
    switch (t.t) {
      case 'begin': {
        const r = await fetch(api(`/api/blobs/${t.hash}/uploads`), { method: 'POST', headers: authHeaders({ 'content-type': 'application/json' }), body: JSON.stringify({ size: t.size }) })
        if (!r.ok) throw new Error(`begin ${r.status}`)
        const v = await r.json()
        return { t: 'began', hash: t.hash, present: !!v.present, uploadId: v.upload_id, received: v.received ?? '' }
      }
      case 'put': {
        const bytes = await readLocal(t.hash, t.offset!, t.len!)
        const r = await fetch(api(`/api/blobs/${t.hash}/uploads/${t.uploadId}/chunks/${t.index}`), { method: 'PUT', headers: authHeaders({ 'x-chunk-sha256': await sha256hex(bytes) }), body: bytes as BodyInit })
        if (r.status === 404) return { t: 'gone', hash: t.hash }
        if (!r.ok) throw new Error(`put ${r.status}`)
        return { t: 'chunk', hash: t.hash, index: t.index, ok: true }
      }
      case 'complete': {
        const r = await fetch(api(`/api/blobs/${t.hash}/uploads/${t.uploadId}/complete`), { method: 'POST', headers: authHeaders() })
        if (r.status === 404) return { t: 'gone', hash: t.hash }
        if (r.status === 422) return { t: 'completed', hash: t.hash, ok: false }
        if (!r.ok) throw new Error(`complete ${r.status}`)
        return { t: 'completed', hash: t.hash, ok: true }
      }
      case 'get': {
        const end = t.offset! + t.len! - 1
        const r = await fetch(api(`/api/blobs/${t.hash}`), { headers: authHeaders({ range: `bytes=${t.offset}-${end}` }) })
        if (!r.ok) return { t: 'range', hash: t.hash, index: t.index, ok: false }
        try {
          await putChunk(db, t.hash, t.index!, new Uint8Array(await r.arrayBuffer()))
        } catch (e) {
          if (isQuota(e)) void onQuota()
          throw e
        }
        return { t: 'range', hash: t.hash, index: t.index, ok: true }
      }
      case 'presence': {
        const r = await fetch(api('/api/blobs/presence'), { method: 'POST', headers: authHeaders({ 'content-type': 'application/json' }), body: JSON.stringify({ hashes: t.hashes }) })
        if (!r.ok) throw new Error(`presence ${r.status}`)
        const v = await r.json()
        const present = new Set<string>(v.present)
        return { t: 'presence', present: [...present], missing: t.hashes!.filter((h) => !present.has(h)) }
      }
    }
  } catch {
    return { t: 'failed', task: t }
  }
  return { t: 'failed', task: t }
}

async function pumpBlobs() {
  for (let round = 0; round < 10000; round++) {
    const pres = core.presenceTask()
    const tasks = JSON.parse(core.blobTasks()) as Parameters<typeof blobTask>[0][]
    if (pres) tasks.push(JSON.parse(pres))
    if (!tasks.length) return
    const results = await Promise.all(tasks.map(blobTask))
    let failed = 0
    await run(() => {
      const writes: [Uint8Array, Uint8Array | null][] = []
      for (const r of results) {
        if ((r as { t: string }).t === 'failed') failed++
        writes.push(...(core.blobResult(JSON.stringify(r), Date.now()) as [Uint8Array, Uint8Array | null][]))
      }
      return { writes, send: [], events: [{ t: 'status' }] }
    })
    if (failed === results.length) {
      setTimeout(schedulePump, 5000)
      return
    }
  }
}

/** Streams a File/Blob into local storage (two passes: hash, then 4 MiB chunks). */
async function ingestBlob(file: Blob): Promise<{ hash: string; size: number; header: Uint8Array }> {
  const h = new Sha256()
  let header = new Uint8Array(0)
  for (let off = 0; off < file.size; off += CHUNK) {
    const b = new Uint8Array(await file.slice(off, off + CHUNK).arrayBuffer())
    if (off === 0) header = b.slice(0, 65536)
    h.update(b)
  }
  const hash = h.finish()
  for (let off = 0, i = 0; off < file.size || (i === 0 && file.size === 0); off += CHUNK, i++) {
    const b = new Uint8Array(await file.slice(off, off + CHUNK).arrayBuffer())
    await putChunk(db, hash, i, b)
    if (file.size === 0) break
  }
  const size = file.size
  await run(() => ({ writes: core.blobIngest(hash, size, Date.now()) as [Uint8Array, Uint8Array | null][], send: [], events: [{ t: 'status' }] }))
  return { hash, size, header }
}

// ---------------------------------------------------------------- docs

async function docUpdates(id: string): Promise<Uint8Array[]> {
  flushDoc(id)
  await queue
  const rows = await scanPrefix(db, core.docPrefix(id, 'body'))
  return rows.concat(core.pendingDocUpdates(id, 'body') as Uint8Array[])
}

async function docText(id: string): Promise<string> {
  const d = new Y.Doc()
  for (const u of await docUpdates(id)) Y.applyUpdate(d, u)
  const s = d.getText('t').toString()
  d.destroy()
  return s
}

function flushDoc(id: string) {
  const c = coalesce.get(id)
  if (!c) return
  clearTimeout(c.timer)
  coalesce.delete(id)
  const merged = c.ups.length === 1 ? c.ups[0] : Y.mergeUpdates(c.ups)
  const n = c.ups.length
  const committed = run(() => core.localDocUpdate(id, 'body', merged, Date.now()) as Output)
  void committed.finally(() => {
    unsaved -= n
    postStatus()
  })
  // The journal entries go once the commit is durable (a failed commit keeps them for replay).
  void committed.then(async () => journalDelete(db, await Promise.all(c.keys))).catch((e) => console.error('jess worker: journal', e))
  markDirty(id)
}

function docUpdate(id: string, update: Uint8Array) {
  received++
  if (unsaved++ === 0) postStatus()
  const key = journalAppend(db, id, update)
  key.catch((e) => console.error('jess worker: journal', e))
  const c = coalesce.get(id)
  if (c) {
    c.ups.push(update)
    c.keys.push(key)
  } else coalesce.set(id, { ups: [update], keys: [key], timer: setTimeout(() => flushDoc(id), 30) })
}

// ---------------------------------------------------------------- index

function markDirty(id: string) {
  dirtyDocs.add(id)
  clearTimeout(indexTimer)
  indexTimer = setTimeout(() => void reindexDirty(), 300)
}

async function ensureIndex(): Promise<SearchIndex> {
  if (index) return index
  if (!indexing) {
    indexing = (async () => {
      index = await SearchIndex.open()
      // Catch up: index every note whose text changed since the index was written.
      const all = JSON.parse(core.viewJson()) as EntryMeta[]
      const live = all.filter((e) => e.kind === 'markdown' && !e.purged && !e.blob)
      const known = index.indexedHashes()
      for (const id of known.keys()) if (!live.some((e) => e.id === id)) index.remove(id)
      let n = 0
      for (const e of live) {
        dirtyDocs.add(e.id)
        if (++n % 200 === 0) post({ ev: 'progress', task: 'index', done: n, total: live.length })
      }
      await reindexDirty()
      post({ ev: 'indexed' })
      schedulePdfIndex(0)
    })()
  }
  await indexing
  return index!
}

// ---------------------------------------------------------------- offline attachments (§7.5)
// `everything`: every attachment in the vault is kept on this device (background prefetch, P3).
// `on-demand`: only what was opened or shown, capped by an LRU (1 GB on the web). Blobs the server
// hasn't confirmed are never evicted (core enforces that).
let offlineMode: 'everything' | 'on-demand' = 'everything'
const ON_DEMAND_CAP = 1 << 30
let policyTimer: ReturnType<typeof setTimeout> | undefined
function schedulePolicy(delay = 3000) {
  clearTimeout(policyTimer)
  policyTimer = setTimeout(() => void storagePolicy(), delay)
}

/** Drops a blob's local bytes if core allows it (the server has confirmed it). */
async function evictOne(h: string): Promise<boolean> {
  const w = core.blobEvict(h) as Write[] | undefined
  if (!w) return false
  await run(() => ({ writes: w, send: [], events: [] }))
  await deleteChunks(db, h)
  for (const k of [...chunkCache.keys()]) if (k.startsWith(`${h}:`)) chunkCache.delete(k)
  return true
}

async function evictTo(cap: number) {
  let n = 0
  for (const h of core.blobEvictionCandidates(cap) as string[]) if (await evictOne(h)) n++
  return n
}

async function storagePolicy() {
  if (!core) return
  if (offlineMode === 'everything') {
    const writes: Write[] = []
    for (const e of JSON.parse(core.viewJson()) as EntryMeta[]) {
      if (!e.blob || e.purged || !e.blobInfo?.size || core.blobIsLocal(e.blob)) continue
      writes.push(...(core.blobWant(e.blob, e.blobInfo.size, 3, Date.now()) as Write[]))
    }
    if (writes.length) await run(() => ({ writes, send: [], events: [] }))
  } else await evictTo(ON_DEMAND_CAP)
  schedulePump()
}

/** Storage is full: switch to on-demand, free space, and tell the UI (a banner). */
async function onQuota() {
  if (offlineMode === 'everything') offlineMode = 'on-demand'
  let cap = ON_DEMAND_CAP / 2
  try {
    const est = await navigator.storage.estimate()
    if (est.usage) cap = Math.min(cap, est.usage * 0.7)
  } catch {
    /* no estimate */
  }
  const n = await evictTo(cap)
  post({ ev: 'quota', evicted: n })
}

const isQuota = (e: unknown) => e instanceof DOMException && (e.name === 'QuotaExceededError' || e.name === 'NS_ERROR_DOM_QUOTA_REACHED')

// PDF text (DESIGN §8): extracted once on the server, fetched here at the lowest priority and
// indexed per page. Not derived yet (404) → try again later.
let pdfTimer: ReturnType<typeof setTimeout> | undefined
let pdfIndexing = false
function schedulePdfIndex(delay = 2000) {
  clearTimeout(pdfTimer)
  pdfTimer = setTimeout(() => void indexPdfs(), delay)
}

async function indexPdfs() {
  if (!index || !token || pdfIndexing) return
  pdfIndexing = true
  let retry = false
  try {
    const live = (JSON.parse(core.viewJson()) as EntryMeta[]).filter((e) => e.kind === 'pdf' && e.blob && !e.purged && !e.trashed)
    const liveIds = new Set(live.map((e) => e.id))
    const known = index.indexedPdfs()
    for (const id of known.keys()) if (!liveIds.has(id)) index.remove(id)
    for (const e of live) {
      if (known.get(e.id) === e.blob) continue
      try {
        const r = await fetch(api(`/api/blobs/${e.blob}/derived/pdf-text`), { headers: authHeaders() })
        if (r.status === 404) {
          retry = true
          continue
        }
        if (!r.ok) throw new Error(String(r.status))
        const v = (await r.json()) as { pages: string[] }
        index.upsertPdf(e.id, e.name, e.blob!, v.pages ?? [])
      } catch {
        retry = true // offline or server trouble
      }
    }
  } finally {
    pdfIndexing = false
  }
  if (retry) schedulePdfIndex(60_000)
}

async function reindexDirty() {
  if (!index) return
  const ids = [...dirtyDocs]
  dirtyDocs.clear()
  if (!ids.length) return
  for (const id of ids) {
    const e = (JSON.parse(core.entriesJson([id])) as EntryMeta[])[0]
    if (!e || 'deleted' in e || e.purged || e.kind !== 'markdown' || e.blob) {
      index.remove(id)
      continue
    }
    const text = await docText(id)
    const ex = JSON.parse(extract(text)) as Extracted
    await index.upsert(id, e.name.replace(/\.md$/i, ''), text, ex)
  }
  // Panels showing index-derived data (backlinks, tags) refresh on this.
  post({ ev: 'indexed' })
}

function nameKeysOf(e: EntryMeta): string[] {
  const k = targetKey(e.name)
  return [k, e.name.normalize('NFC').toLowerCase()]
}

async function backlinks(id: string) {
  const ix = await ensureIndex()
  const e = (JSON.parse(core.entriesJson([id])) as EntryMeta[])[0]
  if (!e || 'deleted' in e) return []
  const cands = ix.linksByKeys([...new Set(nameKeysOf(e))])
  const out = new Map<string, { src: string; count: number; embed: boolean }>()
  for (const l of cands) {
    if (l.src === id) continue
    if (core.resolve(l.target, l.syntax === 'markdown', l.src) !== id) continue
    const o = out.get(l.src)
    if (o) {
      o.count++
      o.embed ||= l.embed
    } else out.set(l.src, { src: l.src, count: 1, embed: l.embed })
  }
  return [...out.values()]
}

/** How many links (from live or trashed notes) resolve to each attachment (DESIGN §7.8). */
async function attachmentRefs(): Promise<Record<string, number>> {
  const ix = await ensureIndex()
  await reindexDirty()
  const atts = (JSON.parse(core.viewJson()) as EntryMeta[]).filter((e) => (e.kind === 'media' || e.kind === 'pdf') && !e.purged)
  const refs: Record<string, number> = {}
  const keys = new Set<string>()
  for (const a of atts) {
    refs[a.id] = 0
    for (const k of nameKeysOf(a)) keys.add(k)
  }
  const cache = new Map<string, string | null>()
  for (const l of ix.linksByKeys([...keys])) {
    const k = `${l.src}\u0000${l.syntax}\u0000${l.target}`
    let r = cache.get(k)
    if (r === undefined) {
      r = core.resolve(l.target, l.syntax === 'markdown', l.src) ?? null
      cache.set(k, r)
    }
    if (r && r in refs) refs[r]++
  }
  return refs
}

// ---------------------------------------------------------------- import / export

let importCancel = false
let importState: { plan: ImportPlan; source: SourceInput } | null = null

type SourceInput = { kind: 'files'; files: File[]; paths: string[] } | { kind: 'zip'; file: File }

interface ImportPlan {
  settings: { attachment_folder_path?: string; new_link_format?: string; use_markdown_links?: boolean }
  folders: string[]
  items: { path: string; kind: 'Markdown' | 'MarkdownBlob' | 'Pdf' | 'Media'; visible: boolean; size: number; mtime?: number; ctime?: number; action: 'Create' | 'SkipSame' | { Conflict: { existing: string; resolution: 'Ask' | 'Overwrite' | 'KeepBoth' | 'Skip' } } }[]
  report: Record<string, unknown>
}

function jsSource(src: SourceInput) {
  const reader = new FileReaderSync()
  if (src.kind === 'zip') {
    return { size: src.file.size, readAt: (off: number, len: number) => new Uint8Array(reader.readAsArrayBuffer(src.file.slice(off, off + len))) }
  }
  const byPath = new Map<string, File>()
  const dirs = new Set<string>()
  src.files.forEach((f, i) => {
    // webkitRelativePath starts with the picked folder's name: strip it.
    const p = src.paths[i].split('/').slice(1).join('/')
    if (!p) return
    byPath.set(p, f)
    let d = p.includes('/') ? p.slice(0, p.lastIndexOf('/')) : ''
    while (d) {
      dirs.add(d)
      d = d.includes('/') ? d.slice(0, d.lastIndexOf('/')) : ''
    }
  })
  return {
    files: [...byPath].map(([path, f]) => ({ path, size: f.size, mtime: f.lastModified })),
    dirs: [...dirs],
    read: (p: string) => new Uint8Array(reader.readAsArrayBuffer(byPath.get(p)!)),
    file: (p: string) => byPath.get(p)!,
  }
}

async function importPlan(src: SourceInput, opts: { hidePdfs?: boolean; conflict?: string }) {
  const js = jsSource(src)
  const texts = new Map<string, Uint8Array>()
  for (const id of core.importCollisions(js) as string[]) texts.set(id, new TextEncoder().encode(await docText(id)))
  const plan = JSON.parse(core.importPlan(js, texts, JSON.stringify({ hidePdfsInAttachmentFolder: opts.hidePdfs ?? true, conflict: opts.conflict ?? 'ask' }))) as ImportPlan
  importState = { plan, source: src }
  return plan
}

function fileOf(js: ReturnType<typeof jsSource>, path: string): Blob {
  return (js as { file(p: string): File }).file(path)
}

/** Executes the planned import in batches of 250 (DESIGN §12.3). */
async function importRun(resolutions: Record<string, 'Overwrite' | 'KeepBoth' | 'Skip'> = {}, applyAll?: 'Overwrite' | 'KeepBoth' | 'Skip') {
  if (!importState) throw new Error('no import planned')
  const { plan, source } = importState
  importCancel = false
  const js = jsSource(source)
  // Zip entries are read through the WASM zip reader (streamed), folder files directly.
  const zipRead = source.kind === 'zip' ? await zipReader(source.file) : null
  const readBytes = (p: string): Uint8Array => (zipRead ? zipRead(p) : new Uint8Array(new FileReaderSync().readAsArrayBuffer(fileOf(js, p))))
  const folderIds = new Map<string, string>()
  const ops: object[] = []
  const flushOps = async () => {
    if (!ops.length) return
    const json = JSON.stringify(ops.splice(0))
    await run(() => core.localMeta(json, Date.now()) as Output)
  }
  const sortedFolders = [...plan.folders].sort((a, b) => a.split('/').length - b.split('/').length || (a < b ? -1 : 1))
  for (const f of sortedFolders) {
    const existing = core.idByPath(f)
    if (existing) {
      folderIds.set(f, existing)
      continue
    }
    const id = newIdStr()
    const parent = f.includes('/') ? folderIds.get(f.slice(0, f.lastIndexOf('/'))) ?? null : null
    ops.push({ op: 'create', id, kind: 'folder', parent, name: f.slice(f.lastIndexOf('/') + 1) })
    folderIds.set(f, id)
    if (ops.length >= 250) await flushOps()
  }
  await flushOps()
  const s = plan.settings
  const sets: object[] = []
  if (s.attachment_folder_path != null) sets.push({ op: 'setProp', id: '00000000-0000-0000-0000-000000000001', key: 'attachmentFolderPath', value: s.attachment_folder_path })
  if (s.new_link_format != null) sets.push({ op: 'setProp', id: '00000000-0000-0000-0000-000000000001', key: 'newLinkFormat', value: s.new_link_format })
  if (s.use_markdown_links != null) sets.push({ op: 'setProp', id: '00000000-0000-0000-0000-000000000001', key: 'useMarkdownLinks', value: s.use_markdown_links })
  if (sets.length) await run(() => core.localMeta(JSON.stringify(sets), Date.now()) as Output).catch(() => {})
  const todo = plan.items.filter((i) => {
    if (i.action === 'SkipSame') return false
    if (typeof i.action === 'object') {
      const r = resolutions[i.path] ?? applyAll ?? (i.action.Conflict.resolution === 'Ask' ? null : i.action.Conflict.resolution)
      if (!r) throw new Error('unresolved conflicts')
      return r !== 'Skip'
    }
    return true
  })
  let done = 0
  for (let b = 0; b < todo.length; b += 250) {
    if (importCancel) break
    const batch = todo.slice(b, b + 250)
    const docs: [string, string, boolean][] = []
    for (const it of batch) {
      const dir = it.path.includes('/') ? it.path.slice(0, it.path.lastIndexOf('/')) : ''
      const parent = dir ? folderIds.get(dir) ?? null : null
      let name = it.path.slice(it.path.lastIndexOf('/') + 1)
      const res = typeof it.action === 'object' ? resolutions[it.path] ?? applyAll ?? it.action.Conflict.resolution : null
      const existing = typeof it.action === 'object' ? it.action.Conflict.existing : null
      if (res === 'KeepBoth') {
        const i = name.lastIndexOf('.')
        name = i > 0 ? `${name.slice(0, i)} (imported)${name.slice(i)}` : `${name} (imported)`
      }
      if (it.kind === 'Markdown') {
        const text = new TextDecoder('utf-8', { fatal: true, ignoreBOM: true }).decode(readBytes(it.path))
        if (res === 'Overwrite' && existing) docs.push([existing, text, true])
        else {
          const id = newIdStr()
          ops.push({ op: 'create', id, kind: 'markdown', parent, name, visible: true, created: it.ctime ?? it.mtime, modified: it.mtime })
          docs.push([id, text, false])
        }
      } else {
        const bytes = readBytes(it.path)
        const { hash, size, header } = await ingestBlob(new Blob([bytes as BlobPart]))
        const info = JSON.parse(wasmBlobInfo(name, header, size))
        if (res === 'Overwrite' && existing) ops.push({ op: 'setBlob', id: existing, blob: hash, blobInfo: info })
        else ops.push({ op: 'create', id: newIdStr(), kind: it.kind === 'Pdf' ? 'pdf' : it.kind === 'MarkdownBlob' ? 'markdown' : 'media', parent, name, visible: it.visible, blob: hash, blobInfo: info, created: it.ctime ?? it.mtime, modified: it.mtime })
      }
    }
    await flushOps()
    for (const [id, text, existing] of docs) {
      const d = new Y.Doc()
      if (existing) for (const u of await docUpdates(id)) Y.applyUpdate(d, u)
      const sv = Y.encodeStateVector(d)
      const t = d.getText('t')
      d.transact(() => {
        if (existing) {
          // Minimal diff: common prefix/suffix.
          const old = t.toString()
          let p = 0
          while (p < old.length && p < text.length && old[p] === text[p]) p++
          let s = 0
          while (s < old.length - p && s < text.length - p && old[old.length - 1 - s] === text[text.length - 1 - s]) s++
          t.delete(p, old.length - p - s)
          t.insert(p, text.slice(p, text.length - s))
        } else t.insert(0, text)
      })
      const u = Y.encodeStateAsUpdate(d, sv)
      d.destroy()
      await run(() => core.localDocUpdate(id, 'body', u, Date.now()) as Output)
      markDirty(id)
    }
    done += batch.length
    post({ ev: 'progress', task: 'import', done, total: todo.length })
  }
  importState = null
  return { ...plan.report, cancelled: importCancel }
}

async function zipReader(file: File): Promise<(p: string) => Uint8Array> {
  // The WASM zip reader extracts one entry at a time, streaming from the File.
  const { ZipReader } = await import('../wasm/core.js')
  const reader = new FileReaderSync()
  const z = new ZipReader({ size: file.size, readAt: (off: number, len: number) => new Uint8Array(reader.readAsArrayBuffer(file.slice(off, off + len))) })
  return (p: string) => z.read(p)
}

function newIdStr(): string {
  const b = new Uint8Array(16)
  crypto.getRandomValues(b.subarray(6))
  let t = Date.now()
  for (let i = 5; i >= 0; i--) {
    b[i] = t % 256
    t = Math.floor(t / 256)
  }
  b[6] = 0x70 | (b[6] & 0x0f)
  b[8] = 0x80 | (b[8] & 0x3f)
  const h = Array.from(b, (x) => x.toString(16).padStart(2, '0')).join('')
  return `${h.slice(0, 8)}-${h.slice(8, 12)}-${h.slice(12, 16)}-${h.slice(16, 20)}-${h.slice(20)}`
}

/** Export: the projection as a stream of files for the main thread's zip writer. */
async function exportFiles(portable: boolean) {
  const p = JSON.parse(core.projection(portable, false)) as { items: { id: string; path: string; kind: string; hash?: string; modified?: number }[]; report: string | null }
  return p
}

async function exportContent(item: { id: string; kind: string; hash?: string }): Promise<Uint8Array> {
  if (item.kind === 'text') return new TextEncoder().encode(await docText(item.id))
  if (item.kind === 'blob') {
    const h = item.hash!
    if (!core.blobIsLocal(h)) {
      const r = await fetch(api(`/api/blobs/${h}`), { headers: authHeaders() })
      if (!r.ok) throw new Error(`blob ${h} unavailable (${r.status})`)
      return new Uint8Array(await r.arrayBuffer())
    }
    const size = (await readSize(h)) ?? 0
    return readLocal(h, 0, size)
  }
  return new Uint8Array()
}

async function blobRange(hash: string, begin: number, end: number): Promise<Uint8Array> {
  const len = end - begin
  // Local chunks when all the needed ones are here (complete or partially downloaded blobs).
  const first = Math.floor(begin / CHUNK)
  const last = Math.floor((end - 1) / CHUNK)
  let local = true
  for (let i = first; i <= last && local; i++) local = !!(await cachedChunk(hash, i))
  if (local) return readLocal(hash, begin, len)
  const r = await fetch(api(`/api/blobs/${hash}`), { headers: authHeaders({ range: `bytes=${begin}-${end - 1}` }) })
  if (!r.ok) throw new Error(`blob range ${r.status}`)
  return new Uint8Array(await r.arrayBuffer())
}

async function blobRead(hash: string, variant: string): Promise<{ bytes: Uint8Array; mime: string | null } | null> {
  if (variant !== 'orig') {
    try {
      const r = await fetch(api(`/api/blobs/${hash}/derived/${variant}`), { headers: authHeaders() })
      if (r.ok) return { bytes: new Uint8Array(await r.arrayBuffer()), mime: r.headers.get('content-type') }
    } catch {
      /* offline: fall through to the original */
    }
    if (variant === 'pdf-thumb' || variant === 'pdf-text') return null
  }
  if (core.blobIsLocal(hash)) {
    const size = (await readSize(hash)) ?? 0
    return { bytes: await readLocal(hash, 0, size), mime: null }
  }
  try {
    const r = await fetch(api(`/api/blobs/${hash}`), { headers: authHeaders() })
    if (!r.ok) return null
    return { bytes: new Uint8Array(await r.arrayBuffer()), mime: r.headers.get('content-type') }
  } catch {
    return null
  }
}

async function readSize(hash: string): Promise<number | null> {
  let size = 0
  for (let i = 0; ; i++) {
    const c = await getChunk(db, hash, i)
    if (!c) break
    size += c.length
    if (c.length < CHUNK) break
  }
  return size
}

/** Commits editor updates a previous session journalled but didn't get to commit (a crash or a
 *  kill inside the coalescing window). Yjs updates are idempotent: one that did get committed
 *  before the crash is harmless to apply again. */
async function replayJournal() {
  const pending = await journalAll(db)
  if (!pending.length) return
  const byDoc = new Map<string, JournalEntry[]>()
  for (const e of pending) byDoc.set(e.id, [...(byDoc.get(e.id) ?? []), e])
  for (const [id, es] of byDoc) {
    const merged = es.length === 1 ? es[0].update : Y.mergeUpdates(es.map((e) => e.update))
    await run(() => core.localDocUpdate(id, 'body', merged, Date.now()) as Output)
    await journalDelete(db, es.map((e) => e.key))
    markDirty(id)
  }
}

// ---------------------------------------------------------------- RPC

const methods: Record<string, (...a: never[]) => unknown> = {
  async init(t: string | null, b: string, replicaHint?: string) {
    token = t
    base = b
    await init()
    db = await openDb()
    const [keys, vals] = await loadAll(db)
    core = new Core(keys, vals, replicaHint ?? randomHex(7), CHUNK)
    await commit(db, core.take_init_writes() as Write[])
    await replayJournal()
    connect()
    setTimeout(() => void ensureIndex(), 1500)
    const r: InitResult = { entries: JSON.parse(core.viewJson()), status: status(), replica: core.replica, vaultId: core.vaultId ?? null }
    return r
  },
  setToken(t: string | null) {
    token = t
    fatal = null
    ws?.close()
    ws = null
    attempt = 0
    if (t) connect()
    postStatus()
  },
  async intent(opsJson: string) {
    try {
      await run(() => core.localMeta(opsJson, Date.now()) as Output)
      return { ok: true }
    } catch (e) {
      return { ok: false, error: String(e) }
    }
  },
  async openDoc(id: string) {
    openDocs.set(id, (openDocs.get(id) ?? 0) + 1)
    return docUpdates(id)
  },
  closeDoc(id: string) {
    const n = (openDocs.get(id) ?? 1) - 1
    if (n <= 0) openDocs.delete(id)
    else openDocs.set(id, n)
    flushDoc(id)
  },
  docUpdate(id: string, update: Uint8Array) {
    docUpdate(id, update)
  },
  async flush() {
    for (const id of [...coalesce.keys()]) flushDoc(id)
    await queue
  },
  async docText(id: string) {
    return docText(id)
  },
  resolve(target: string, markdown: boolean, src: string | null) {
    return core.resolve(target, markdown, src ?? undefined) ?? null
  },
  async backlinks(id: string) {
    await ensureIndex()
    await reindexDirty()
    return backlinks(id)
  },
  async tags() {
    const ix = await ensureIndex()
    await reindexDirty()
    return ix.tags()
  },
  async notesWithTag(t: string) {
    return (await ensureIndex()).notesWithTag(t)
  },
  async search(q: string) {
    const ix = await ensureIndex()
    await reindexDirty()
    return ix.search(q)
  },
  status() {
    return status()
  },
  quarantine() {
    return JSON.parse(core.quarantineJson())
  },
  setForeground(f: boolean) {
    foreground = f
    if (f) probe()
  },
  online() {
    probe()
  },
  offline() {
    // The OS says the network is gone: show it now rather than after the heartbeat timeout.
    dropSocket()
    scheduleReconnect()
  },
  async ingest(file: Blob, name?: string) {
    const r = await ingestBlob(file)
    const info = name ? JSON.parse(wasmBlobInfo(name, r.header, r.size)) : { size: r.size }
    return { ...r, info }
  },
  /** Queue a download (priorities §7.4: 0 open, 1 embed, 2 recent, 3 prefetch). */
  blobWant(hash: string, size: number, prio: number) {
    void run(() => ({ writes: core.blobWant(hash, size, prio, Date.now()) as Write[], send: [], events: [] }))
    schedulePump()
  },
  /** Bytes [begin, end) of a blob for PDF.js's range transport. */
  attachmentRefs() {
    return attachmentRefs()
  },
  setOfflineMode(mode: 'everything' | 'on-demand') {
    offlineMode = mode
    schedulePolicy(mode === 'everything' ? 5000 : 0)
  },
  async blobRange(hash: string, begin: number, end: number): Promise<Uint8Array> {
    return blobRange(hash, begin, end)
  },
  blobIsLocal(hash: string) {
    return core.blobIsLocal(hash)
  },
  /** Bytes of a blob or a derived variant: local first, else an authenticated fetch. */
  async blobRead(hash: string, variant: string): Promise<{ bytes: Uint8Array; mime: string | null } | null> {
    return blobRead(hash, variant)
  },
  async importPlan(src: SourceInput, opts: { hidePdfs?: boolean; conflict?: string }) {
    return importPlan(src, opts)
  },
  async importRun(res: Record<string, 'Overwrite' | 'KeepBoth' | 'Skip'>, all?: 'Overwrite' | 'KeepBoth' | 'Skip') {
    return importRun(res, all)
  },
  importCancel() {
    importCancel = true
  },
  exportFiles(portable: boolean) {
    return exportFiles(portable)
  },
  exportContent(item: { id: string; kind: string; hash?: string }) {
    return exportContent(item)
  },
  async metaGet(k: string) {
    return metaGet(db, k)
  },
  async dropBlob(hash: string) {
    return evictOne(hash)
  },
}

// Calls that arrive before `init` finishes (the UI renders from the boot record first) wait for
// it, in arrival order.
let markReady: () => void
let markFailed: (e: unknown) => void
const ready = new Promise<void>((res, rej) => {
  markReady = res
  markFailed = rej
})
ready.catch(() => {})

self.onmessage = async (m: MessageEvent<Req>) => {
  const { id, method, args } = m.data
  const f = methods[method] as ((...a: unknown[]) => unknown) | undefined
  if (!f) {
    self.postMessage({ id, ok: false, error: `unknown method ${method}` })
    return
  }
  try {
    if (method === 'init') {
      try {
        const value = await f(...args)
        markReady()
        self.postMessage({ id, ok: true, value })
      } catch (e) {
        markFailed(e)
        throw e
      }
      return
    }
    await ready
    const value = await f(...args)
    self.postMessage({ id, ok: true, value })
  } catch (e) {
    self.postMessage({ id, ok: false, error: e instanceof Error ? e.message : String(e) })
  }
}
