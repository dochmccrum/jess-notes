// Service worker: offline app shell (DESIGN §11.7). Hashed assets are precached from a list
// injected at build time (scripts/sw-manifest.mjs); API traffic is never touched.
/// <reference lib="webworker" />
declare const self: ServiceWorkerGlobalScope

const RAW: string = '__JESS_PRECACHE__'
const PRECACHE: { version: string; files: string[] } = RAW.startsWith('{') ? JSON.parse(RAW) : { version: 'dev', files: [] }
const CACHE = `jess-shell-${PRECACHE.version}`

self.addEventListener('install', (e) => {
  e.waitUntil(
    (async () => {
      const c = await caches.open(CACHE)
      await c.addAll(['/', ...PRECACHE.files.map((f) => `/${f}`)])
      await self.skipWaiting()
    })(),
  )
})

self.addEventListener('activate', (e) => {
  e.waitUntil(
    (async () => {
      for (const k of await caches.keys()) if (k.startsWith('jess-shell-') && k !== CACHE) await caches.delete(k)
      await self.clients.claim()
    })(),
  )
})

self.addEventListener('fetch', (e) => {
  const req = e.request
  if (req.method !== 'GET') return
  const url = new URL(req.url)
  if (url.origin !== self.location.origin) return
  if (url.pathname.startsWith('/_blob/')) {
    e.respondWith(serveBlob(req, url))
    return
  }
  if (url.pathname.startsWith('/api/') || url.pathname === '/healthz') return
  if (req.mode === 'navigate') {
    // Network first with a short timeout so a new deploy shows up; the cached shell offline.
    e.respondWith(
      (async () => {
        const cache = await caches.open(CACHE)
        try {
          const ctrl = new AbortController()
          const t = setTimeout(() => ctrl.abort(), 2500)
          const r = await fetch(req, { signal: ctrl.signal })
          clearTimeout(t)
          if (r.ok) await cache.put('/', r.clone())
          return r
        } catch {
          return (await cache.match('/')) ?? Response.error()
        }
      })(),
    )
    return
  }
  e.respondWith(
    (async () => {
      const hit = await caches.match(req)
      if (hit) return hit
      const r = await fetch(req)
      if (r.ok && url.pathname.startsWith('/assets/')) {
        const c = await caches.open(CACHE)
        await c.put(req, r.clone())
      }
      return r
    })(),
  )
})

// ------------------------------------------------------------------ /_blob/{hash}/{variant}
// Authenticated attachment bytes for <img> (DESIGN §7.7): local chunks from IndexedDB first,
// otherwise the server with the device token (read from IndexedDB; never in a URL). Derived
// variants (display/thumb/pdf-thumb) are small and kept in Cache Storage.

const CHUNK = 4 << 20
const DERIVED_CACHE = 'jess-derived-v1'
let tokenCache: string | null = null

const DB_NAME = 'jess'
let dbP: Promise<IDBDatabase> | null = null

// One shared connection. The sync worker owns the schema (src/worker/idb.ts): this opens whatever
// version exists, so it never fails or blocks when the app upgrades it, and it closes when the
// database is upgraded or deleted (the next request reopens it). If the service worker gets there
// first, it creates version 1 with the stores it reads; the worker's upgrade adds the rest.
function openDb(): Promise<IDBDatabase> {
  if (dbP) return dbP
  const p = new Promise<IDBDatabase>((resolve, reject) => {
    const r = indexedDB.open(DB_NAME)
    r.onupgradeneeded = () => {
      const db = r.result
      for (const s of ['kv', 'blobchunks', 'meta']) if (!db.objectStoreNames.contains(s)) db.createObjectStore(s)
    }
    r.onsuccess = () => {
      const db = r.result
      const forget = () => {
        if (dbP === p) dbP = null
      }
      db.onversionchange = () => {
        forget()
        db.close()
      }
      db.onclose = forget
      resolve(db)
    }
    r.onerror = () => {
      if (dbP === p) dbP = null
      reject(r.error)
    }
  })
  dbP = p
  return p
}

function idbGet<T>(db: IDBDatabase, store: string, key: string): Promise<T | undefined> {
  return new Promise((resolve, reject) => {
    const r = db.transaction(store, 'readonly').objectStore(store).get(key)
    r.onsuccess = () => resolve(r.result as T | undefined)
    r.onerror = () => reject(r.error)
  })
}

const chunkKey = (hash: string, i: number) => `${hash}:${String(i).padStart(8, '0')}`

async function token(db: IDBDatabase): Promise<string | null> {
  if (!tokenCache) tokenCache = (await idbGet<string>(db, 'meta', 'token')) ?? null
  return tokenCache
}

async function authFetch(db: IDBDatabase, path: string, headers: Record<string, string> = {}): Promise<Response> {
  for (let attempt = 0; attempt < 2; attempt++) {
    const t = await token(db)
    const r = await fetch(path, { headers: { ...headers, ...(t ? { authorization: `Bearer ${t}` } : {}) } })
    if (r.status !== 401 || attempt) return r
    tokenCache = null // the token changed (re-login): read it again
  }
  throw new Error('unreachable')
}

function parseRange(h: string | null, size: number): [number, number] | null {
  const m = h && /^bytes=(\d*)-(\d*)$/.exec(h.trim())
  if (!m) return null
  let a: number
  let b: number
  if (m[1] === '') {
    a = Math.max(0, size - Number(m[2]))
    b = size - 1
  } else {
    a = Number(m[1])
    b = m[2] === '' ? size - 1 : Math.min(Number(m[2]), size - 1)
  }
  return a <= b && a < size ? [a, b] : null
}

const withSvgSandbox = (mime: string, h: Headers) => {
  // SVG is only ever shown through <img>; if opened directly it can't run scripts.
  if (mime.includes('svg')) h.set('content-security-policy', 'sandbox')
  return h
}

async function localResponse(db: IDBDatabase, hash: string, size: number, mime: string, rangeHeader: string | null): Promise<Response | null> {
  if (!size) return null
  const range = parseRange(rangeHeader, size)
  const [a, b] = range ?? [0, size - 1]
  const first = Math.floor(a / CHUNK)
  const last = Math.floor(b / CHUNK)
  // Every needed chunk must be here, or the whole request goes to the network.
  const have = await Promise.all(
    Array.from({ length: last - first + 1 }, (_, k) =>
      new Promise<boolean>((resolve) => {
        const r = db.transaction('blobchunks', 'readonly').objectStore('blobchunks').count(chunkKey(hash, first + k))
        r.onsuccess = () => resolve(r.result > 0)
        r.onerror = () => resolve(false)
      }),
    ),
  )
  if (have.some((x) => !x)) return null
  let i = first
  const body = new ReadableStream<Uint8Array>({
    async pull(ctrl) {
      if (i > last) return ctrl.close()
      const buf = await idbGet<ArrayBuffer>(db, 'blobchunks', chunkKey(hash, i))
      if (!buf) return ctrl.error(new Error('chunk vanished'))
      const c = new Uint8Array(buf)
      const s = i === first ? a - i * CHUNK : 0
      const e = i === last ? b - i * CHUNK + 1 : c.length
      ctrl.enqueue(c.subarray(s, e))
      i++
    },
  })
  const h = withSvgSandbox(mime, new Headers({ 'content-type': mime, 'content-length': String(b - a + 1), 'accept-ranges': 'bytes', 'cache-control': 'no-store' }))
  if (range) h.set('content-range', `bytes ${a}-${b}/${size}`)
  return new Response(body, { status: range ? 206 : 200, headers: h })
}

async function serveBlob(req: Request, url: URL): Promise<Response> {
  const [, , hash, variant = 'orig'] = url.pathname.split('/')
  if (!/^[0-9a-f]{64}$/.test(hash)) return new Response('bad hash', { status: 400 })
  const size = Number(url.searchParams.get('s') ?? 0)
  const mime = url.searchParams.get('t') || 'application/octet-stream'
  try {
    const db = await openDb()
    if (variant !== 'orig') {
      const cache = await caches.open(DERIVED_CACHE)
      const key = `/_derived/${hash}/${variant}`
      const hit = await cache.match(key)
      if (hit) return hit
      try {
        const r = await authFetch(db, `/api/blobs/${hash}/derived/${variant}`)
        if (r.ok) {
          const h = new Headers(r.headers)
          h.set('cache-control', 'no-store')
          const res = new Response(await r.arrayBuffer(), { status: 200, headers: h })
          await cache.put(key, res.clone())
          return res
        }
      } catch {
        /* offline */
      }
      // Not derived (yet): the original serves as its own display variant.
      if (variant === 'pdf-thumb') return new Response('not derived', { status: 404 })
    }
    const local = await localResponse(db, hash, size, mime, req.headers.get('range'))
    if (local) return local
    const range = req.headers.get('range')
    const r = await authFetch(db, `/api/blobs/${hash}`, range ? { range } : {})
    const h = withSvgSandbox(mime, new Headers(r.headers))
    if (mime !== 'application/octet-stream') h.set('content-type', mime)
    return new Response(r.body, { status: r.status, headers: h })
  } catch {
    return new Response('offline', { status: 503 })
  }
}

export {}
