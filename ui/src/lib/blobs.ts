// BlobUrlResolver (DESIGN §7.7): URLs for attachment bytes that <img> can use. With an active
// service worker that's `/_blob/{hash}/{variant}` (served locally or fetched with the device
// token). Without one (first visit, dev server, or no SW support) the bytes come through the
// backend and become object URLs, revoked by an LRU.
import type { Backend } from '../backend/types'
import type { BlobInfo } from './types'

export type Variant = 'orig' | 'display' | 'thumb' | 'pdf-thumb'

const MAX_OBJECT_URLS = 150
const objectUrls = new Map<string, string>() // key → object URL (insertion order = LRU)

function swActive(): boolean {
  return typeof navigator !== 'undefined' && !!navigator.serviceWorker?.controller
}

export function swUrl(hash: string, variant: Variant, info?: BlobInfo | null): string {
  const q = new URLSearchParams()
  if (info?.size) q.set('s', String(info.size))
  if (info?.mime) q.set('t', info.mime)
  const qs = q.toString()
  return `/_blob/${hash}/${variant}${qs ? `?${qs}` : ''}`
}

/** A URL for the blob, or null if its bytes can't be had right now (offline and not local). */
export async function blobUrl(backend: Backend, hash: string, variant: Variant, info?: BlobInfo | null): Promise<string | null> {
  if (swActive()) return swUrl(hash, variant, info)
  const key = `${hash}/${variant}`
  const hit = objectUrls.get(key)
  if (hit) {
    objectUrls.delete(key)
    objectUrls.set(key, hit)
    return hit
  }
  const r = await backend.blobRead(hash, variant)
  if (!r) return null
  const type = r.mime && !r.mime.startsWith('application/octet-stream') ? r.mime : (info?.mime ?? '')
  const url = URL.createObjectURL(new Blob([r.bytes as BlobPart], { type }))
  objectUrls.set(key, url)
  while (objectUrls.size > MAX_OBJECT_URLS) {
    const [k, u] = objectUrls.entries().next().value as [string, string]
    objectUrls.delete(k)
    URL.revokeObjectURL(u)
  }
  return url
}
