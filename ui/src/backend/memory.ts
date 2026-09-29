// MemoryBackend: an in-process Backend for component tests (DESIGN §11.2).
import * as Y from 'yjs'
import { EntryStore } from '../stores/entries'
import { writable } from '../lib/store'
import type { Backend, BackendEvent, DocSession } from './types'
import type { EntryMeta, MetaIntent, SyncStatus } from '../lib/types'

export class MemoryBackend implements Backend {
  readonly entries = new EntryStore()
  readonly sync = writable<SyncStatus>({ state: 'synced' })
  readonly texts = new Map<string, string>()
  readonly log: MetaIntent[][] = []
  private seq = 0

  constructor(initial: EntryMeta[] = []) {
    this.entries.load(initial)
  }

  async start() {}

  async intent(ops: MetaIntent[]) {
    this.log.push(ops)
    const changed: EntryMeta[] = []
    for (const o of ops) {
      const cur = 'id' in o ? this.entries.get(o.id) : undefined
      switch (o.op) {
        case 'create':
          changed.push({ id: o.id, kind: o.kind, parent: o.parent, name: o.name, trashed: null, visible: o.visible ?? true, blob: o.blob ?? null, created: o.created ?? null, modified: null, purged: false, seq: ++this.seq, props: {} })
          break
        case 'setName':
          if (cur) changed.push({ ...cur, name: o.name, seq: ++this.seq })
          break
        case 'setParent':
          if (cur) changed.push({ ...cur, parent: o.parent, seq: ++this.seq })
          break
        case 'setVisible':
          if (cur) changed.push({ ...cur, visible: o.visible, seq: ++this.seq })
          break
        case 'trash':
          if (cur) {
            const t = { batch: o.id, at: Date.now() }
            changed.push({ ...cur, trashed: t, seq: ++this.seq })
            for (const d of this.entries.descendants(o.id)) changed.push({ ...this.entries.get(d)!, trashed: t })
          }
          break
        default:
          break
      }
    }
    this.entries.apply(changed)
    return { ok: true }
  }

  async openDoc(id: string): Promise<DocSession> {
    const ydoc = new Y.Doc()
    ydoc.getText('t').insert(0, this.texts.get(id) ?? '')
    return { id, ydoc, dispose: () => ydoc.destroy() }
  }
  async docText(id: string) {
    return this.texts.get(id) ?? ''
  }
  async backlinks() {
    return []
  }
  async tags() {
    return []
  }
  async notesWithTag() {
    return []
  }
  async search() {
    return []
  }
  async ingest(file: Blob) {
    return { hash: '0'.repeat(64), size: file.size, header: new Uint8Array() }
  }
  importer = {
    plan: async () => ({ report: { notes: 0, pdfs: 0, pdfs_hidden: 0, images: 0, other_media: 0, folders: 0, unchanged: 0, conflicts: 0, skipped: [], unresolved: [], collisions: [], warnings: [] }, items: [], settings: {} }),
    run: async () => ({}),
    cancel: () => {},
  }
  exporter = {
    files: async () => ({ items: [], report: null }),
    content: async () => new Uint8Array(),
  }
  on(_cb: (e: BackendEvent) => void) {
    return () => {}
  }
  setForeground() {}
  online() {}
  offline() {}
  async flush() {}
  setToken() {}
  async quarantine() {
    return []
  }
}
