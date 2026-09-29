// "Erase this device's local copy" (Settings): wipes IndexedDB (notes, queue, blobs, token) and
// OPFS (search index) on the next load, before anything opens them. Device settings are kept.
import { DB_NAME } from '../worker/idb'

const FLAG = 'jess.erase'

export function requestErase() {
  localStorage.setItem(FLAG, '1')
  location.hash = ''
  location.reload()
}

export async function eraseIfRequested(): Promise<void> {
  if (localStorage.getItem(FLAG) !== '1') return
  await new Promise<void>((resolve, reject) => {
    const r = indexedDB.deleteDatabase(DB_NAME)
    r.onsuccess = () => resolve()
    r.onerror = () => reject(r.error)
    r.onblocked = () => resolve() // another tab still has it open; the tab gate prevents that
  })
  try {
    const root = await navigator.storage.getDirectory()
    const names: string[] = []
    for await (const [name] of (root as unknown as { entries(): AsyncIterable<[string, FileSystemHandle]> }).entries()) names.push(name)
    for (const n of names) await root.removeEntry(n, { recursive: true })
  } catch {
    /* no OPFS */
  }
  try {
    for (const k of await caches.keys()) if (k.startsWith('jess-derived')) await caches.delete(k)
  } catch {
    /* no Cache Storage */
  }
  try {
    const dev = JSON.parse(localStorage.getItem('jess.device') ?? '{}')
    delete dev.lastNote
    delete dev.expanded
    localStorage.setItem('jess.device', JSON.stringify(dev))
  } catch {
    /* ignore */
  }
  localStorage.removeItem(FLAG)
}
