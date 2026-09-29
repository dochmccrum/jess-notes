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

export {}
