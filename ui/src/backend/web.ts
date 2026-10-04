// WebBackend: the sync worker (core WASM + Yjs + IndexedDB) behind the Backend interface.
import * as Y from 'yjs'
import { EntryStore } from '../stores/entries'
import { writable } from '../lib/store'
import type { Backend, BackendEvent, DocSession } from './types'
import type { BlobInfo, MetaIntent, SyncStatus } from '../lib/types'
import type { InitResult, Res, WorkerEvent } from '../worker/protocol'

export class WebBackend implements Backend {
  readonly entries = new EntryStore()
  readonly sync = writable<SyncStatus>({ state: 'starting' })
  private worker: Worker
  private seq = 0
  private calls = new Map<number, [(v: unknown) => void, (e: Error) => void]>()
  private docs = new Map<string, Set<Y.Doc>>()
  private listeners = new Set<(e: BackendEvent) => void>()
  private ready: Promise<InitResult> | null = null

  constructor(
    private token: string | null,
    private base = '',
  ) {
    this.worker = new Worker(new URL('../worker/sync.worker.ts', import.meta.url), { type: 'module', name: 'jess-sync' })
    this.worker.onmessage = (m: MessageEvent<Res | WorkerEvent>) => this.onMessage(m.data)
  }

  /** Doc updates posted to the worker; its status counts those it has received (`docUpdates`). A
   *  status from before the worker saw the latest keystrokes isn't "synced" (DESIGN §22). */
  private sentUpdates = 0
  private workerStatus: SyncStatus = { state: 'starting' }
  private honest(s: SyncStatus): SyncStatus {
    return s.state === 'synced' && (s.docUpdates ?? 0) < this.sentUpdates ? { ...s, state: 'syncing' } : s
  }

  private onMessage(d: Res | WorkerEvent) {
    if ('id' in d && typeof d.id === 'number' && 'ok' in d) {
      const c = this.calls.get(d.id)
      if (!c) return
      this.calls.delete(d.id)
      if (d.ok) c[0](d.value)
      else c[1](new Error(d.error))
      return
    }
    const e = d as WorkerEvent
    switch (e.ev) {
      case 'entries':
        this.entries.apply(e.list)
        break
      case 'doc':
        for (const doc of this.docs.get(e.id) ?? []) Y.applyUpdate(doc, e.update, 'remote')
        break
      case 'status':
        this.workerStatus = e.status
        this.sync.set(this.honest(e.status))
        break
      default:
        for (const l of this.listeners) l(e as BackendEvent)
    }
  }

  private call<T>(method: string, ...args: unknown[]): Promise<T> {
    const id = ++this.seq
    return new Promise<T>((res, rej) => {
      this.calls.set(id, [res as (v: unknown) => void, rej])
      this.worker.postMessage({ id, method, args })
    })
  }

  async start() {
    if (!this.ready) this.ready = this.call<InitResult>('init', this.token, this.base)
    const r = await this.ready
    this.entries.load(r.entries)
    this.workerStatus = r.status
    this.sync.set(this.honest(r.status))
  }

  intent(ops: MetaIntent[]) {
    return this.call<{ ok: boolean; error?: string }>('intent', JSON.stringify(ops))
  }

  /** The last-open note's state from the boot record (DESIGN §11.7): its first `openDoc` shows
   *  it at once, before the worker has loaded the vault. */
  private boot: { id: string; state: Uint8Array } | null = null
  preload(id: string, state: Uint8Array) {
    this.boot = { id, state }
  }

  /** The open note's state, for the boot record (null if it isn't open). */
  docState(id: string): Uint8Array | null {
    const d = this.docs.get(id)?.values().next().value
    return d ? Y.encodeStateAsUpdate(d) : null
  }

  async openDoc(id: string): Promise<DocSession> {
    const ydoc = new Y.Doc()
    let set = this.docs.get(id)
    if (!set) this.docs.set(id, (set = new Set()))
    set.add(ydoc)
    const boot = this.boot?.id === id ? this.boot.state : null
    this.boot = null
    const loaded = this.call<Uint8Array[]>('openDoc', id).then((updates) => {
      if (ydoc.isDestroyed) return
      Y.transact(ydoc, () => {
        for (const u of updates) Y.applyUpdate(ydoc, u, 'remote')
      }, 'remote')
      if (!boot) return
      // Whatever the boot state has that the worker's rows don't (normally nothing) goes to the
      // worker like an edit, so nothing shown can be lost.
      const sv = updates.length ? Y.encodeStateVectorFromUpdate(Y.mergeUpdates(updates)) : new Uint8Array([0])
      const missing = Y.encodeStateAsUpdate(ydoc, sv)
      const d = Y.decodeUpdate(missing)
      if (d.structs.length || d.ds.clients.size) onUpdate(missing, 'boot-diff')
    })
    const onUpdate = (u: Uint8Array, origin: unknown) => {
      if (origin === 'remote' || origin === 'boot') return
      this.sentUpdates++
      this.worker.postMessage({ id: 0, method: 'docUpdate', args: [id, u] })
      this.sync.set(this.honest(this.workerStatus))
    }
    ydoc.on('update', onUpdate)
    if (boot) {
      Y.applyUpdate(ydoc, boot, 'boot')
      loaded.catch((e) => console.error('jess: opening the note failed', e))
    } else await loaded
    return {
      id,
      ydoc,
      dispose: () => {
        ydoc.off('update', onUpdate)
        set!.delete(ydoc)
        void this.call('closeDoc', id)
        ydoc.destroy()
      },
    }
  }

  docText(id: string) {
    return this.call<string>('docText', id)
  }
  backlinks(id: string) {
    return this.call<{ src: string; count: number; embed: boolean }[]>('backlinks', id)
  }
  tags() {
    return this.call<{ name: string; srcs: string[] }[]>('tags')
  }
  notesWithTag(t: string) {
    return this.call<string[]>('notesWithTag', t)
  }
  search(q: string) {
    return this.call<{ id: string; snippet: string; page?: number }[]>('search', q)
  }
  ingest(file: Blob, name?: string) {
    return this.call<{ hash: string; size: number; header: Uint8Array; info: BlobInfo }>('ingest', file, name)
  }
  blobWant(hash: string, size: number, prio: number) {
    this.worker.postMessage({ id: 0, method: 'blobWant', args: [hash, size, prio] })
  }
  attachmentRefs() {
    return this.call<Record<string, number>>('attachmentRefs')
  }
  blobRange(hash: string, begin: number, end: number) {
    return this.call<Uint8Array>('blobRange', hash, begin, end)
  }
  blobRead(hash: string, variant: string) {
    return this.call<{ bytes: Uint8Array; mime: string | null } | null>('blobRead', hash, variant)
  }
  importer = {
    plan: (src: Parameters<Backend['importer']['plan']>[0], opts: { hidePdfs?: boolean; conflict?: string }) => this.call<Awaited<ReturnType<Backend['importer']['plan']>>>('importPlan', src, opts),
    run: (res: Record<string, 'Overwrite' | 'KeepBoth' | 'Skip'>, all?: 'Overwrite' | 'KeepBoth' | 'Skip') => this.call<Record<string, unknown>>('importRun', res, all),
    cancel: () => void this.call('importCancel'),
  }
  exporter = {
    files: (portable: boolean) => this.call<Awaited<ReturnType<Backend['exporter']['files']>>>('exportFiles', portable),
    content: (item: Parameters<Backend['exporter']['content']>[0]) => this.call<Uint8Array>('exportContent', item),
  }
  on(cb: (e: BackendEvent) => void) {
    this.listeners.add(cb)
    return () => this.listeners.delete(cb)
  }
  setOfflineMode(mode: 'everything' | 'on-demand') {
    void this.call('setOfflineMode', mode)
  }
  /** Writes the boot record in the worker, from core's view (see `saveBoot` there). */
  saveBoot(lastNote: string | null, doc: Uint8Array | null) {
    return this.call<void>('saveBoot', lastNote, doc)
  }
  setForeground(f: boolean) {
    void this.call('setForeground', f)
  }
  online() {
    void this.call('online')
  }
  offline() {
    void this.call('offline')
  }
  flush() {
    return this.call<void>('flush')
  }
  setToken(t: string | null) {
    this.token = t
    void this.call('setToken', t)
  }
  quarantine() {
    return this.call<unknown[]>('quarantine')
  }
}
