// IndexedDB persistence for the sync worker. `kv` holds the core's key-value store (binary keys,
// compared bytewise); `blobchunks` holds local blob bytes in 4 MiB records; `meta` holds device
// settings, the token and the cold-start boot record (read by the main thread directly);
// `journal` holds editor updates from the moment the worker receives them until their coalesced
// commit lands (DESIGN §22 item 61).

export const DB_NAME = 'jess'
const VERSION = 2

const open = new Map<string, Promise<IDBDatabase>>()

/** One connection per database and realm (page, worker, service worker), shared by every caller.
 *  It closes itself when another context upgrades or deletes the database, so a newer version of
 *  the app (or "erase this device") is never blocked by an open connection; the next call reopens. */
export function openDb(name = DB_NAME): Promise<IDBDatabase> {
  let p = open.get(name)
  if (p) return p
  p = new Promise<IDBDatabase>((resolve, reject) => {
    const r = indexedDB.open(name, VERSION)
    r.onupgradeneeded = () => {
      const db = r.result
      if (!db.objectStoreNames.contains('kv')) db.createObjectStore('kv')
      if (!db.objectStoreNames.contains('blobchunks')) db.createObjectStore('blobchunks')
      if (!db.objectStoreNames.contains('meta')) db.createObjectStore('meta')
      if (!db.objectStoreNames.contains('journal')) db.createObjectStore('journal', { autoIncrement: true })
    }
    r.onsuccess = () => {
      const db = r.result
      const forget = () => {
        if (open.get(name) === p) open.delete(name)
      }
      db.onversionchange = () => {
        forget()
        db.close()
      }
      db.onclose = forget
      const close = db.close.bind(db)
      db.close = () => {
        forget()
        close()
      }
      resolve(db)
    }
    r.onerror = () => {
      if (open.get(name) === p) open.delete(name)
      reject(r.error)
    }
  })
  open.set(name, p)
  return p
}

export const toKey = (u: Uint8Array): ArrayBuffer => u.slice().buffer as ArrayBuffer

function req<T>(r: IDBRequest<T>): Promise<T> {
  return new Promise((res, rej) => {
    r.onsuccess = () => res(r.result)
    r.onerror = () => rej(r.error)
  })
}

function done(tx: IDBTransaction): Promise<void> {
  return new Promise((res, rej) => {
    tx.oncomplete = () => res()
    tx.onerror = () => rej(tx.error)
    tx.onabort = () => rej(tx.error ?? new Error('transaction aborted'))
  })
}

/** Loads the whole KV store (keys and values in key order). */
/** Note text rows (`u` prefix, core::kv::P_DOC): core only indexes their keys at load, and the
 *  worker reads a note's rows when it is opened, so their values (most of the store) aren't read. */
const DOC_PREFIX = 0x75

/** Every key, with every value except note text rows' (empty there). */
export async function loadAll(db: IDBDatabase): Promise<[Uint8Array[], Uint8Array[]]> {
  const tx = db.transaction('kv', 'readonly')
  const s = tx.objectStore('kv')
  const docs = new Uint8Array([DOC_PREFIX]).buffer
  const after = new Uint8Array([DOC_PREFIX + 1]).buffer
  const [keys, lo, hi] = await Promise.all([req(s.getAllKeys()), req(s.getAll(IDBKeyRange.upperBound(docs, true))), req(s.getAll(IDBKeyRange.lowerBound(after)))])
  const empty = new Uint8Array(0)
  const vals: Uint8Array[] = []
  let i = 0
  let j = 0
  const ks = keys.map((k) => new Uint8Array(k as ArrayBuffer))
  for (const k of ks) {
    if (k[0] === DOC_PREFIX) vals.push(empty)
    else if (k[0] < DOC_PREFIX || k.length === 0) vals.push(new Uint8Array(lo[i++] as ArrayBuffer))
    else vals.push(new Uint8Array(hi[j++] as ArrayBuffer))
  }
  return [ks, vals]
}

export type Write = [Uint8Array, Uint8Array | null]

/** Commits writes atomically, in order (one readwrite transaction). */
export async function commit(db: IDBDatabase, writes: Write[]): Promise<void> {
  if (!writes.length) return
  const tx = db.transaction('kv', 'readwrite', { durability: 'strict' } as IDBTransactionOptions)
  const s = tx.objectStore('kv')
  for (const [k, v] of writes) {
    if (v) s.put(toKey(v), toKey(k))
    else s.delete(toKey(k))
  }
  await done(tx)
}

function successor(p: Uint8Array): Uint8Array | null {
  const u = p.slice()
  for (let i = u.length - 1; i >= 0; i--) {
    if (u[i] < 0xff) {
      u[i]++
      return u.slice(0, i + 1)
    }
  }
  return null
}

export function prefixRange(prefix: Uint8Array): IDBKeyRange {
  const up = successor(prefix)
  return up ? IDBKeyRange.bound(toKey(prefix), toKey(up), false, true) : IDBKeyRange.lowerBound(toKey(prefix))
}

/** Values under a key prefix, in key order. */
export async function scanPrefix(db: IDBDatabase, prefix: Uint8Array): Promise<Uint8Array[]> {
  const tx = db.transaction('kv', 'readonly')
  const v = await req(tx.objectStore('kv').getAll(prefixRange(prefix)))
  return v.map((x) => new Uint8Array(x as ArrayBuffer))
}

export async function metaGet<T>(db: IDBDatabase, key: string): Promise<T | undefined> {
  const tx = db.transaction('meta', 'readonly')
  return (await req(tx.objectStore('meta').get(key))) as T | undefined
}

export async function metaPut(db: IDBDatabase, key: string, value: unknown): Promise<void> {
  const tx = db.transaction('meta', 'readwrite')
  tx.objectStore('meta').put(value, key)
  await done(tx)
}

export async function metaDelete(db: IDBDatabase, key: string): Promise<void> {
  const tx = db.transaction('meta', 'readwrite')
  tx.objectStore('meta').delete(key)
  await done(tx)
}

// ---------------------------------------------------------------- blob chunks

const chunkKey = (hash: string, i: number) => `${hash}:${String(i).padStart(8, '0')}`

export async function putChunk(db: IDBDatabase, hash: string, index: number, bytes: Uint8Array): Promise<void> {
  const tx = db.transaction('blobchunks', 'readwrite')
  tx.objectStore('blobchunks').put(toKey(bytes), chunkKey(hash, index))
  await done(tx)
}

/** Several chunks in one transaction (a download round's small attachments). */
export async function putChunks(db: IDBDatabase, chunks: { hash: string; index: number; bytes: Uint8Array }[]): Promise<void> {
  if (!chunks.length) return
  const tx = db.transaction('blobchunks', 'readwrite')
  const st = tx.objectStore('blobchunks')
  for (const c of chunks) st.put(toKey(c.bytes), chunkKey(c.hash, c.index))
  await done(tx)
}

export async function getChunk(db: IDBDatabase, hash: string, index: number): Promise<Uint8Array | null> {
  const tx = db.transaction('blobchunks', 'readonly')
  const v = await req(tx.objectStore('blobchunks').get(chunkKey(hash, index)))
  return v ? new Uint8Array(v as ArrayBuffer) : null
}

export async function deleteChunks(db: IDBDatabase, hash: string): Promise<void> {
  const tx = db.transaction('blobchunks', 'readwrite')
  tx.objectStore('blobchunks').delete(IDBKeyRange.bound(`${hash}:`, `${hash}:~`))
  await done(tx)
}

// ---------------------------------------------------------------- journal

export interface JournalEntry {
  key: number
  id: string
  update: Uint8Array
}

/** Appends an editor update; resolves with its key once it's durable. */
export async function journalAppend(db: IDBDatabase, id: string, update: Uint8Array): Promise<number> {
  const tx = db.transaction('journal', 'readwrite')
  const key = await req(tx.objectStore('journal').add({ id, update }))
  await done(tx)
  return key as number
}

export async function journalDelete(db: IDBDatabase, keys: number[]): Promise<void> {
  if (!keys.length) return
  const tx = db.transaction('journal', 'readwrite')
  const st = tx.objectStore('journal')
  for (const k of keys) st.delete(k)
  await done(tx)
}

/** Every journalled update, oldest first. */
export async function journalAll(db: IDBDatabase): Promise<JournalEntry[]> {
  const tx = db.transaction('journal', 'readonly')
  const st = tx.objectStore('journal')
  const [keys, vals] = await Promise.all([req(st.getAllKeys()), req(st.getAll())])
  return keys.map((k, i) => ({ key: k as number, id: (vals[i] as { id: string }).id, update: new Uint8Array((vals[i] as { update: Uint8Array }).update) }))
}

