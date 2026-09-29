// PDF embeds (DESIGN §10.4): `![[f.pdf]]`, `#page=3`, `#page=3&height=600`. A static first-page
// thumbnail (server-rendered) is shown first; within one viewport of the screen it becomes a live
// compact viewer. At most 2 live embeds exist (LRU); one more than 2 viewports away is torn down.
import type { RendererImpl } from './renderers'
import { env } from './renderers'
import type { PdfCtx } from './embeds'
import { blobUrl } from '../lib/blobs'
import type { PdfViewer } from '../pdf/viewer'

const MAX_LIVE = 2
const live: { el: HTMLElement; stop(): void }[] = []

function makeLive(el: HTMLElement, box: HTMLElement, ctx: PdfCtx): () => void {
  let viewer: PdfViewer | null = null
  let dead = false
  const host = document.createElement('div')
  host.className = 'jess-pdf-embed-scroll'
  void import('../pdf/viewer').then(async ({ openPdf, PdfViewer }) => {
    try {
      const doc = await openPdf({ backend: env!.backend, hash: ctx.hash!, size: ctx.info?.size ?? 0 })
      if (dead) return void doc.destroy()
      box.replaceChildren(host)
      viewer = new PdfViewer(host, doc, { page: ctx.page, zoom: 'fit', compact: true })
      await viewer.init()
    } catch (e) {
      if (!dead) box.dataset.error = (e as Error).message
    }
  })
  const entry = {
    el,
    stop: () => {
      dead = true
      viewer?.destroy()
      viewer = null
    },
  }
  live.push(entry)
  while (live.length > MAX_LIVE) live.shift()!.stop()
  return () => {
    const i = live.indexOf(entry)
    if (i >= 0) live.splice(i, 1)
    entry.stop()
  }
}

export const impl: RendererImpl = {
  render(ctx0, el) {
    const ctx = ctx0 as unknown as PdfCtx
    const height = ctx.height ?? 520
    const wrap = document.createElement('div')
    wrap.className = 'jess-pdf-embed'
    wrap.style.height = `${height}px`
    const head = document.createElement('div')
    head.className = 'jess-pdf-embed-head'
    const title = document.createElement('span')
    title.textContent = `📄 ${ctx.source}${ctx.page > 1 ? ` · page ${ctx.page}` : ''}`
    const open = document.createElement('button')
    open.className = 'btn small'
    open.textContent = 'Open'
    open.onclick = (e) => {
      e.preventDefault()
      if (ctx.entryId) env?.openEntry(ctx.entryId, `#page=${ctx.page}`)
    }
    head.append(title, open)
    const box = document.createElement('div')
    box.className = 'jess-pdf-embed-box'
    wrap.append(head, box)
    el.append(wrap)
    if (ctx.state !== 'ok' || !ctx.hash || !env) {
      box.textContent = ctx.state === 'preparing' ? `Adding ${ctx.source}…` : ctx.state === 'unresolved' ? `⚠ ${ctx.source} not found` : `⚠ ${ctx.source} is missing`
      return
    }
    // Static thumbnail of page 1 (other pages render once live).
    if (ctx.page === 1) {
      void blobUrl(env.backend, ctx.hash, 'pdf-thumb', ctx.info).then((u) => {
        if (!u || box.childElementCount) return
        const img = document.createElement('img')
        img.alt = ctx.source
        img.src = u
        img.onerror = () => img.remove()
        box.append(img)
      })
    }
    let stop: (() => void) | null = null
    const near = new IntersectionObserver((es) => {
      if (es.some((e) => e.isIntersecting) && !stop) stop = makeLive(el, box, ctx)
    }, { rootMargin: '100% 0px' })
    const far = new IntersectionObserver((es) => {
      if (es.every((e) => !e.isIntersecting) && stop) {
        stop()
        stop = null
      }
    }, { rootMargin: '200% 0px' })
    near.observe(wrap)
    far.observe(wrap)
    return () => {
      near.disconnect()
      far.disconnect()
      stop?.()
    }
  },
}
