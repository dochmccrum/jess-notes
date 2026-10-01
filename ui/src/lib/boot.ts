// Cold-start record (DESIGN §11.7): last-open note + a compact entries snapshot, read with one
// IndexedDB get (web) or one Rust command reading SQLite (Tauri) before the backend is ready.
import { openDb, metaGet, metaPut } from '../worker/idb'
import type { Space } from './spaces'
import { isTauri, tauriInvoke } from './platform'
import type { EntryMeta } from './types'

export interface BootRecord {
  lastNote: string | null
  entries: EntryMeta[]
  doc: Uint8Array | null
  at: number
}

export async function readBoot(space: Space): Promise<BootRecord | null> {
  try {
    if (isTauri) {
      const s = await tauriInvoke<string | null>('meta_get', { key: 'boot' })
      return s ? (JSON.parse(s) as BootRecord) : null
    }
    const db = await openDb(`jess-space-${space.id.replace(/[^a-zA-Z0-9_-]/g, '_')}`)
    return ((await metaGet<BootRecord>(db, 'boot')) ?? null) as BootRecord | null
  } catch {
    return null
  }
}

export async function writeBoot(b: BootRecord, space: Space): Promise<void> {
  try {
    if (isTauri) return await tauriInvoke('meta_put', { key: 'boot', value: JSON.stringify({ ...b, doc: null }) })
    const db = await openDb(`jess-space-${space.id.replace(/[^a-zA-Z0-9_-]/g, '_')}`)
    await metaPut(db, 'boot', b)
  } catch {
    /* best effort */
  }
}
