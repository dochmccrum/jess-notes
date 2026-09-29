// The only thing components talk to (DESIGN §11.2).
import type * as Y from 'yjs'
import type { EntryStore } from '../stores/entries'
import type { BlobInfo, MetaIntent, SearchHit, SyncStatus } from '../lib/types'
import type { Readable } from '../lib/store'

export interface DocSession {
  id: string
  ydoc: Y.Doc
  dispose(): void
}

export interface Backlink {
  src: string
  count: number
  embed: boolean
}

export type ImportSource = { kind: 'files'; files: File[]; paths: string[] } | { kind: 'zip'; file: File }

export interface ImportPlanView {
  report: {
    notes: number
    pdfs: number
    pdfs_hidden: number
    images: number
    other_media: number
    folders: number
    unchanged: number
    conflicts: number
    skipped: [string, string][]
    unresolved: [string, string][]
    collisions: string[]
    warnings: string[]
  }
  items: { path: string; kind: string; action: unknown }[]
  settings: Record<string, unknown>
}

export interface ExportItem {
  id: string
  path: string
  kind: 'dir' | 'text' | 'blob'
  hash?: string
  modified?: number
}

export type BackendEvent =
  | { ev: 'progress'; task: string; done: number; total: number }
  | { ev: 'rejected'; opId: number; reason: string }
  | { ev: 'fatal'; message: string }
  | { ev: 'indexed' }
  | { ev: 'quota'; evicted: number }

/** Native file access (Tauri): real folder/zip paths instead of browser file lists. */
export interface NativeIO {
  pickFolder(title: string): Promise<string | null>
  pickZip(title: string): Promise<string | null>
  pickSaveZip(defaultName: string): Promise<string | null>
  importPlanPath(kind: 'folder' | 'zip', path: string, opts: { hidePdfs?: boolean; conflict?: string }): Promise<ImportPlanView>
  exportTo(path: string, portable: boolean, zip: boolean): Promise<void>
}

export interface Backend {
  readonly entries: EntryStore
  /** Present on native platforms. */
  readonly native?: NativeIO
  readonly sync: Readable<SyncStatus>
  start(): Promise<void>
  intent(ops: MetaIntent[]): Promise<{ ok: boolean; error?: string }>
  openDoc(id: string): Promise<DocSession>
  docText(id: string): Promise<string>
  backlinks(id: string): Promise<Backlink[]>
  tags(): Promise<{ name: string; srcs: string[] }[]>
  notesWithTag(tag: string): Promise<string[]>
  search(q: string): Promise<{ id: string; snippet: string; page?: number }[]>
  /** Streams bytes into local storage; with a name, also reads its facts (mime, dimensions). */
  ingest(file: Blob, name?: string): Promise<{ hash: string; size: number; header: Uint8Array; info: BlobInfo }>
  /** Queue a blob download at a priority (0 open doc, 1 embed, 2 recent, 3 prefetch). */
  blobWant(hash: string, size: number, prio: number): void
  /** Bytes of a blob (`orig`) or derived variant, local first; null if unavailable. */
  blobRange(hash: string, begin: number, end: number): Promise<Uint8Array>
  /** Link counts per attachment id (live and trashed notes). */
  attachmentRefs(): Promise<Record<string, number>>
  blobRead(hash: string, variant: string): Promise<{ bytes: Uint8Array; mime: string | null } | null>
  importer: {
    plan(src: ImportSource, opts: { hidePdfs?: boolean; conflict?: string }): Promise<ImportPlanView>
    run(resolutions: Record<string, 'Overwrite' | 'KeepBoth' | 'Skip'>, applyAll?: 'Overwrite' | 'KeepBoth' | 'Skip'): Promise<Record<string, unknown>>
    cancel(): void
  }
  exporter: {
    files(portable: boolean): Promise<{ items: ExportItem[]; report: string | null }>
    content(item: ExportItem): Promise<Uint8Array>
  }
  on(cb: (e: BackendEvent) => void): () => void
  setForeground(f: boolean): void
  /** Device setting (§7.5): keep every attachment offline, or only what's used (LRU-capped). */
  setOfflineMode(mode: 'everything' | 'on-demand'): void
  online(): void
  offline(): void
  flush(): Promise<void>
  setToken(t: string | null): void
  quarantine(): Promise<unknown[]>
}

export type { SearchHit, BlobInfo }
