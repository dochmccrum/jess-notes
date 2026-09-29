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
        this.sync.set(e.status)
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
    this.sync.set(r.status)
  }

  intent(ops: MetaIntent[]) {
    return this.call<{ ok: boolean; error?: string }>('intent', JSON.stringify(ops))
  }

  async openDoc(id: string): Promise<DocSession> {
    const ydoc = new Y.Doc()
    let set = this.docs.get(id)
    if (!set) this.docs.set(id, (set = new Set()))
    set.add(ydoc)
    const updates = await this.call<Uint8Array[]>('openDoc', id)
    Y.transact(ydoc, () => {
      for (const u of updates) Y.applyUpdate(ydoc, u, 'remote')
    }, 'remote')
    const onUpdate = (u: Uint8Array, origin: unknown) => {
      if (origin !== 'remote') this.worker.postMessage({ id: 0, method: 'docUpdate', args: [id, u] })
    }
    ydoc.on('update', onUpdate)
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
    return this.call<{ id: string; snippet: string }[]>('search', q)
  }
  ingest(file: Blob, name?: string) {
    return this.call<{ hash: string; size: number; header: Uint8Array; info: BlobInfo }>('ingest', file, name)
  }
  blobWant(hash: string, size: number, prio: number) {
    this.worker.postMessage({ id: 0, method: 'blobWant', args: [hash, size, prio] })
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
