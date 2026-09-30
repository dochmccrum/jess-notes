// Shared types between the UI, the worker and the backends.

export interface Trashed {
  batch: string
  at: number
}

export interface EntryMeta {
  id: string
  kind: string // folder | markdown | pdf | media | vault | future kinds
  parent: string | null
  name: string
  trashed: Trashed | null
  visible: boolean
  blob: string | null
  created: number | null
  modified: number | null
  purged: boolean
  seq: number
  props: Record<string, unknown>
  /** What is known about the blob (from the server's blob row or this device's ingest). */
  blobInfo?: BlobInfo
}

export type MetaIntent =
  | { op: 'create'; id: string; kind: string; parent: string | null; name: string; visible?: boolean; blob?: string; blobInfo?: BlobInfo; created?: number; modified?: number }
  | { op: 'setParent'; id: string; parent: string | null }
  | { op: 'setName'; id: string; name: string }
  | { op: 'setVisible'; id: string; visible: boolean }
  | { op: 'setBlob'; id: string; blob: string; blobInfo?: BlobInfo }
  | { op: 'trash'; id: string }
  | { op: 'restore'; target: string }
  | { op: 'purge'; id: string }
  | { op: 'setProp'; id: string; key: string; value: string | number | boolean | null }
  | { op: 'setTimes'; id: string; created?: number; modified?: number }

export interface BlobInfo {
  size: number
  mime?: string | null
  width?: number | null
  height?: number | null
  orientation?: number | null
}

export interface SyncStatus {
  state: 'synced' | 'syncing' | 'offline' | 'error' | 'starting'
  pending?: number
  error?: string
  uploads?: { pending: number; total: number }
  downloads?: number
  quarantined?: number
  /** Web worker: doc updates received from the UI so far (see `WebBackend.honest`). */
  docUpdates?: number
}

export interface LinkInfo {
  syntax: 'wiki' | 'markdown'
  embed: boolean
  target: string
  raw_target: string
  subpath?: string
  display?: string
  angle?: boolean
  range: [number, number]
  target_range: [number, number]
}

export interface Extracted {
  links: LinkInfo[]
  tags: { name: string; range?: [number, number] }[]
  frontmatter?: unknown
  aliases?: string[]
  math?: { range: [number, number]; display: boolean }[]
}

export interface SearchHit {
  id: string
  path: string
  snippet: string
  page?: number
}

export const VAULT_SETTINGS_ID = '00000000-0000-0000-0000-000000000001'

export const isDocumentKind = (k: string) => k === 'markdown' || k === 'pdf'
export const isLinkable = (e: EntryMeta) => !e.trashed && !e.purged && e.kind !== 'folder' && e.kind !== 'vault'
export const isLive = (e: EntryMeta) => !e.trashed && !e.purged
