// Main thread ↔ sync worker messages.
import type { EntryMeta, SyncStatus } from '../lib/types'

export type Req = { id: number; method: string; args: unknown[] }
export type Res = { id: number; ok: true; value: unknown } | { id: number; ok: false; error: string }

export type WorkerEvent =
  | { ev: 'entries'; list: (EntryMeta | { id: string; deleted: true })[] }
  | { ev: 'doc'; id: string; update: Uint8Array }
  | { ev: 'status'; status: SyncStatus }
  | { ev: 'fatal'; message: string }
  | { ev: 'rejected'; opId: number; reason: string }
  | { ev: 'progress'; task: string; done: number; total: number; label?: string }
  | { ev: 'indexed' }

export interface InitResult {
  entries: EntryMeta[]
  status: SyncStatus
  replica: string
  vaultId: string | null
}
