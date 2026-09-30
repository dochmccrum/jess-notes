// TauriBackend (DESIGN §11.2): the same Backend over Tauri IPC. The Rust side (jess-native) runs
// the same core with SQLite and file blobs; events arrive on a Channel in the worker's shapes.
// Binary payloads (doc updates, blob bytes) travel as raw IPC bodies, not JSON.
import * as Y from 'yjs'
import { invoke, Channel } from '@tauri-apps/api/core'
import { EntryStore } from '../stores/entries'
import { writable } from '../lib/store'
import { isAndroid } from '../lib/platform'
import type { Backend, BackendEvent, DocSession, ImportPlanView, NativeIO } from './types'
import type { BlobInfo, EntryMeta, MetaIntent, SyncStatus } from '../lib/types'

type NativeEvent =
  | { ev: 'entries'; list: (EntryMeta | { id: string; deleted: true })[] }
  | { ev: 'doc'; id: string; update: number[] }
  | { ev: 'status'; status: SyncStatus }
  | (BackendEvent & { ev: string })

/** `[u32 LE length][bytes]…` → the parts. */
function unframe(buf: ArrayBuffer): Uint8Array[] {
  const out: Uint8Array[] = []
  const v = new DataView(buf)
  let i = 0
  while (i + 4 <= buf.byteLength) {
    const n = v.getUint32(i, true)
    out.push(new Uint8Array(buf, i + 4, n))
    i += 4 + n
  }
  return out
}

/** Android's pickers reject on cancel ("File picker cancelled"); desktop ones return null. */
function cancelled(e: unknown): null {
  if (/cancel/i.test(String(e))) return null
  throw e
}

/** Bytes as an IPC body. Android's IPC only carries JSON (its WebView can't read request
 *  bodies), where a Uint8Array would become an array of numbers: base64 is several times
 *  smaller and faster to parse. Rust's `raw_body` accepts either. */
function rawArg(b: Uint8Array): Uint8Array | { b64: string } {
  if (!isAndroid) return b
  let s = ''
  for (let i = 0; i < b.length; i += 0x8000) s += String.fromCharCode.apply(null, b.subarray(i, i + 0x8000) as unknown as number[])
  return { b64: btoa(s) }
}

export class TauriBackend implements Backend {
  readonly entries = new EntryStore()
  readonly sync = writable<SyncStatus>({ state: 'starting' })
  /** `doc_update` calls not yet answered: until then the native status doesn't include them. */
  private inFlight = 0
  private nativeStatus: SyncStatus = { state: 'starting' }
  private honest(s: SyncStatus): SyncStatus {
    return s.state === 'synced' && this.inFlight > 0 ? { ...s, state: 'syncing' } : s
  }
  private docs = new Map<string, Set<Y.Doc>>()
  private listeners = new Set<(e: BackendEvent) => void>()
  private ready: Promise<void> | null = null

  private onEvent(e: NativeEvent) {
    switch (e.ev) {
      case 'entries':
        this.entries.apply((e as { list: (EntryMeta | { id: string; deleted: true })[] }).list)
        break
      case 'doc': {
        const d = e as { id: string; update: number[] }
        const u = new Uint8Array(d.update)
        for (const doc of this.docs.get(d.id) ?? []) Y.applyUpdate(doc, u, 'remote')
        break
      }
      case 'status':
        this.nativeStatus = (e as { status: SyncStatus }).status
        this.sync.set(this.honest(this.nativeStatus))
        break
      default:
        for (const l of this.listeners) l(e as BackendEvent)
    }
  }

  async start() {
    if (!this.ready) {
      this.ready = (async () => {
        // Events emitted after the snapshot can arrive before init's reply: hold them until the
        // snapshot is loaded, then replay them in order (they're ordered, so the last one wins).
        let held: NativeEvent[] | null = []
        const ch = new Channel<NativeEvent>()
        ch.onmessage = (e) => (held ? held.push(e) : this.onEvent(e))
        const r = await invoke<{ entries: EntryMeta[]; status: SyncStatus }>('init', { onEvent: ch })
        this.entries.load(r.entries)
        this.nativeStatus = r.status
        this.sync.set(this.honest(r.status))
        const replay = held
        held = null
        for (const e of replay) this.onEvent(e)
      })()
    }
    await this.ready
  }

  async intent(ops: MetaIntent[]) {
    try {
      await invoke('intent', { ops: JSON.stringify(ops) })
      return { ok: true }
    } catch (e) {
      return { ok: false, error: String(e) }
    }
  }

  async openDoc(id: string): Promise<DocSession> {
    const ydoc = new Y.Doc()
    let set = this.docs.get(id)
    if (!set) this.docs.set(id, (set = new Set()))
    set.add(ydoc)
    const buf = await invoke<ArrayBuffer>('open_doc', { id })
    Y.transact(ydoc, () => {
      for (const u of unframe(buf)) Y.applyUpdate(ydoc, u, 'remote')
    }, 'remote')
    const onUpdate = (u: Uint8Array, origin: unknown) => {
      if (origin === 'remote') return
      this.inFlight++
      this.sync.set(this.honest(this.nativeStatus))
      void invoke('doc_update', rawArg(u), { headers: { 'x-id': id } }).finally(() => {
        this.inFlight--
        this.sync.set(this.honest(this.nativeStatus))
      })
    }
    ydoc.on('update', onUpdate)
    return {
      id,
      ydoc,
      dispose: () => {
        ydoc.off('update', onUpdate)
        set!.delete(ydoc)
        void invoke('close_doc', { id })
        ydoc.destroy()
      },
    }
  }

  docText(id: string) {
    return invoke<string>('doc_text', { id })
  }
  backlinks(id: string) {
    return invoke<{ src: string; count: number; embed: boolean }[]>('backlinks', { id })
  }
  tags() {
    return invoke<{ name: string; srcs: string[] }[]>('tags')
  }
  notesWithTag(tag: string) {
    return invoke<string[]>('notes_with_tag', { tag })
  }
  search(q: string) {
    return invoke<{ id: string; snippet: string; page?: number }[]>('search', { q })
  }
  async ingest(file: Blob, name?: string) {
    const bytes = new Uint8Array(await file.arrayBuffer())
    const r = await invoke<{ hash: string; size: number; info: BlobInfo }>('ingest', rawArg(bytes), { headers: { 'x-name': encodeURIComponent(name ?? '') } })
    return { ...r, header: bytes.slice(0, 65536) }
  }
  blobWant(hash: string, size: number, prio: number) {
    void invoke('blob_want', { hash, size, prio })
  }
  async blobRange(hash: string, begin: number, end: number) {
    return new Uint8Array(await invoke<ArrayBuffer>('blob_range', { hash, begin, end }))
  }
  attachmentRefs() {
    return invoke<Record<string, number>>('attachment_refs')
  }
  async blobRead(hash: string, variant: string) {
    const buf = await invoke<ArrayBuffer>('blob_read', { hash, variant })
    // [mime length u32][mime][bytes]; an empty reply means unavailable.
    if (!buf.byteLength) return null
    const n = new DataView(buf).getUint32(0, true)
    const mime = n ? new TextDecoder().decode(new Uint8Array(buf, 4, n)) : null
    return { bytes: new Uint8Array(buf, 4 + n), mime }
  }
  importer = {
    plan: async (): Promise<ImportPlanView> => {
      throw new Error('use native.importPlanPath on this platform')
    },
    run: (resolutions: Record<string, 'Overwrite' | 'KeepBoth' | 'Skip'>, applyAll?: 'Overwrite' | 'KeepBoth' | 'Skip') => invoke<Record<string, unknown>>('import_run', { resolutions, applyAll: applyAll ?? null }),
    cancel: () => void invoke('import_cancel'),
  }
  exporter = {
    files: async () => ({ items: [], report: null }),
    content: async () => new Uint8Array(),
  }
  readonly native: NativeIO = {
    pickFolder: async (title) => {
      const { open } = await import('@tauri-apps/plugin-dialog')
      const r = await open({ directory: true, title })
      return typeof r === 'string' ? r : null
    },
    pickZip: async (title) => {
      const { open } = await import('@tauri-apps/plugin-dialog')
      const r = await open({ title, filters: [{ name: 'Zip archive', extensions: ['zip'] }] }).catch(cancelled)
      return typeof r === 'string' ? r : null
    },
    pickSaveZip: async (defaultPath) => {
      const { save } = await import('@tauri-apps/plugin-dialog')
      return (await save({ defaultPath, filters: [{ name: 'Zip archive', extensions: ['zip'] }] }).catch(cancelled)) ?? null
    },
    importPlanPath: (kind, path, opts) => invoke<ImportPlanView>('import_plan', { kind, path, hidePdfs: opts.hidePdfs ?? true, conflict: opts.conflict ?? 'ask' }),
    exportTo: (path, portable, zip) => invoke('export_to', { path, portable, zip }),
  }
  on(cb: (e: BackendEvent) => void) {
    this.listeners.add(cb)
    return () => this.listeners.delete(cb)
  }
  setForeground(f: boolean) {
    void invoke('set_foreground', { foreground: f })
  }
  setOfflineMode(mode: 'everything' | 'on-demand') {
    void invoke('set_offline_mode', { everything: mode === 'everything' })
  }
  online() {
    void invoke('online')
  }
  offline() {
    // The native transport notices by itself (heartbeats); nothing to do.
  }
  flush() {
    return invoke<void>('flush')
  }
  setToken(t: string | null) {
    void invoke('set_token', { token: t })
  }
  quarantine() {
    return invoke<unknown[]>('quarantine')
  }
}
