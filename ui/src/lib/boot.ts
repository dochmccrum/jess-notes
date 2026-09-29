// Cold-start record (DESIGN §11.7): last-open note + its merged doc state + a compact entries
// snapshot, read with one IndexedDB get before the worker is ready.
import { openDb, metaGet, metaPut } from '../worker/idb'
import type { EntryMeta } from './types'

export interface BootRecord {
  lastNote: string | null
  entries: EntryMeta[]
  doc: Uint8Array | null
  at: number
}

export async function readBoot(): Promise<BootRecord | null> {
  try {
    const db = await openDb()
    return ((await metaGet<BootRecord>(db, 'boot')) ?? null) as BootRecord | null
  } catch {
    return null
  }
}

export async function writeBoot(b: BootRecord): Promise<void> {
  try {
    const db = await openDb()
    await metaPut(db, 'boot', b)
  } catch {
    /* best effort */
  }
}
