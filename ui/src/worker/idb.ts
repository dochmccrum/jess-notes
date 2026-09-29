// IndexedDB persistence for the sync worker. `kv` holds the core's key-value store (binary keys,
// compared bytewise); `blobchunks` holds local blob bytes in 4 MiB records; `meta` holds device
// settings, the token and the cold-start boot record (read by the main thread directly).

export const DB_NAME = 'jess'
const VERSION = 1

export function openDb(name = DB_NAME): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    const r = indexedDB.open(name, VERSION)
    r.onupgradeneeded = () => {
      const db = r.result
      if (!db.objectStoreNames.contains('kv')) db.createObjectStore('kv')
      if (!db.objectStoreNames.contains('blobchunks')) db.createObjectStore('blobchunks')
      if (!db.objectStoreNames.contains('meta')) db.createObjectStore('meta')
    }
    r.onsuccess = () => resolve(r.result)
    r.onerror = () => reject(r.error)
  })
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
export async function loadAll(db: IDBDatabase): Promise<[Uint8Array[], Uint8Array[]]> {
  const tx = db.transaction('kv', 'readonly')
  const s = tx.objectStore('kv')
  const [keys, vals] = await Promise.all([req(s.getAllKeys()), req(s.getAll())])
  return [keys.map((k) => new Uint8Array(k as ArrayBuffer)), vals.map((v) => new Uint8Array(v as ArrayBuffer))]
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
