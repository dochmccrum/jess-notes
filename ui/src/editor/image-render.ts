// Image embeds (DESIGN §10.4): <img width height> sized from the blob's facts so nothing shifts
// when the bytes arrive; the display variant (≤1600 px) is used inline, the original in the viewer.
// Distinct placeholders for unresolved, preparing, missing and offline.
import type { RendererImpl } from './renderers'
import { env } from './renderers'
import { imageBox, type ImageCtx } from './embeds'
import { blobUrl } from '../lib/blobs'

function placeholder(el: HTMLElement, kind: string, text: string, box: { width: number; height: number } | null) {
  el.replaceChildren()
  const p = document.createElement('span')
  p.className = `jess-img-placeholder ${kind}`
  p.textContent = text
  if (box) {
    p.style.width = `${box.width}px`
    p.style.aspectRatio = `${box.width} / ${box.height}`
  }
  el.append(p)
}

export const impl: RendererImpl = {
  render(ctx0, el) {
    const ctx = ctx0 as unknown as ImageCtx
    const box = imageBox(ctx)
    el.classList.add('jess-img')
    if (ctx.state === 'unresolved') return placeholder(el, 'unresolved', `⚠ ${ctx.source} not found`, null)
    if (ctx.state === 'preparing') return placeholder(el, 'preparing', `Adding ${ctx.source}…`, box)
    if (ctx.state === 'missing' || (!ctx.hash && !ctx.remote)) return placeholder(el, 'missing', `⚠ ${ctx.source} is missing`, box)
    const img = document.createElement('img')
    img.decoding = 'async'
    img.loading = 'lazy'
    img.alt = ctx.alt || ctx.source
    img.draggable = false
    if (box) {
      img.width = box.width
      img.height = box.height
    } else if (ctx.spec) img.width = ctx.spec[0]
    img.style.maxWidth = '100%'
    img.style.height = 'auto'
    let dead = false
    img.onerror = () => {
      if (!dead) placeholder(el, 'offline', `${ctx.source} isn't available offline`, box)
    }
    img.addEventListener('click', (e) => {
      e.preventDefault()
      env?.openImage(ctx)
    })
    el.append(img)
    if (ctx.remote) {
      img.referrerPolicy = 'no-referrer'
      img.src = ctx.remote
    } else if (env) {
      // Keep it offline-available once it has been shown (on-demand devices), at embed priority.
      if (ctx.info?.size) env.backend.blobWant(ctx.hash!, ctx.info.size, 1)
      void blobUrl(env.backend, ctx.hash!, ctx.info?.mime === 'image/svg+xml' || ctx.info?.mime === 'image/gif' ? 'orig' : 'display', ctx.info).then((u) => {
        if (dead) return
        if (u) img.src = u
        else placeholder(el, 'offline', `${ctx.source} isn't available offline`, box)
      })
    }
    return () => {
      dead = true
    }
  },
}
