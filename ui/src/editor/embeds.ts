// Which renderer an embed uses, and its context (DESIGN §10.4). Pure: resolution happens in the
// caller; this maps the resolved entry (and what's known about its blob) to a renderer.
import type { EntryMeta, BlobInfo } from '../lib/types'
import { isPending } from './attachments'

export const IMAGE_EXT = /\.(png|jpe?g|gif|webp|bmp|svg|avif|heic|heif|tiff?)$/i
export const PDF_EXT = /\.pdf$/i

export interface ImageCtx {
  source: string
  state: 'ok' | 'unresolved' | 'preparing' | 'missing'
  hash?: string
  info?: BlobInfo | null
  remote?: string
  spec?: [number, number | null] | null
  alt?: string
  entryId?: string
}

export interface PdfCtx {
  source: string
  state: 'ok' | 'unresolved' | 'preparing' | 'missing'
  hash?: string
  info?: BlobInfo | null
  entryId?: string
  page: number
  height: number | null
}

/** `300` or `300x200` (Obsidian size spec). */
export function parseSize(s: string | null | undefined): [number, number | null] | null {
  const m = s ? /^\s*(\d{1,5})(?:\s*x\s*(\d{1,5}))?\s*$/.exec(s) : null
  return m ? [Number(m[1]), m[2] ? Number(m[2]) : null] : null
}

/** `#page=3&height=600` */
export function parsePdfSubpath(sub: string | null): { page: number; height: number | null } {
  const q = new URLSearchParams((sub ?? '').replace(/^#/, ''))
  const page = Math.max(1, Number(q.get('page')) || 1)
  const h = Number(q.get('height'))
  return { page, height: h > 0 ? Math.min(h, 4000) : null }
}

/** Oriented display dimensions (EXIF orientations 5–8 swap width and height). */
export function orientedSize(info?: BlobInfo | null): [number, number] | null {
  if (!info?.width || !info?.height) return null
  return (info.orientation ?? 1) >= 5 ? [info.height, info.width] : [info.width, info.height]
}

/** The on-screen box for an image: explicit size spec, else natural size; capped at `maxW`. */
export function imageBox(ctx: Pick<ImageCtx, 'spec' | 'info'>, maxW = 760): { width: number; height: number } | null {
  const nat = orientedSize(ctx.info)
  let w: number
  let h: number
  if (ctx.spec) {
    w = ctx.spec[0]
    h = ctx.spec[1] ?? (nat ? Math.round((w * nat[1]) / nat[0]) : 0)
    if (!h) return null
  } else if (nat) {
    ;[w, h] = nat
  } else return null
  if (w > maxW) {
    h = Math.round((h * maxW) / w)
    w = maxW
  }
  return { width: w, height: h }
}

const isRemote = (t: string) => /^https?:\/\//i.test(t)

export function embedFor(
  target: string,
  entry: EntryMeta | null,
  subpath: string | null,
  display: string | null,
): { id: string; ctx: Record<string, unknown> } {
  // Markdown alt text may carry a size: `![alt|300](…)`.
  let alt = display ?? ''
  let spec = parseSize(display)
  if (!spec && display?.includes('|')) {
    const i = display.lastIndexOf('|')
    spec = parseSize(display.slice(i + 1))
    if (spec) alt = display.slice(0, i)
  } else if (spec) alt = ''
  const name = target.split('/').pop() ?? target
  if (!entry) {
    if (isRemote(target)) return { id: 'image', ctx: { source: target, state: 'ok', remote: target, spec, alt } satisfies ImageCtx }
    const state = isPending(name) ? 'preparing' : 'unresolved'
    if (PDF_EXT.test(target)) return { id: 'pdf-embed', ctx: { source: target, state, ...parsePdfSubpath(subpath) } satisfies PdfCtx }
    if (IMAGE_EXT.test(target)) return { id: 'image', ctx: { source: target, state, spec, alt } satisfies ImageCtx }
    return { id: 'transclusion', ctx: { source: target, subpath } }
  }
  const mime = entry.blobInfo?.mime ?? ''
  const base = { source: entry.name, hash: entry.blob ?? undefined, info: entry.blobInfo ?? null, entryId: entry.id, state: entry.blob ? 'ok' : 'missing' } as const
  if (entry.kind === 'pdf' || mime === 'application/pdf') return { id: 'pdf-embed', ctx: { ...base, ...parsePdfSubpath(subpath) } satisfies PdfCtx }
  if (mime.startsWith('image/') || IMAGE_EXT.test(entry.name)) return { id: 'image', ctx: { ...base, spec, alt } satisfies ImageCtx }
  if (entry.kind === 'markdown') return { id: 'transclusion', ctx: { source: target, subpath } }
  return { id: 'file-embed', ctx: { ...base } }
}
