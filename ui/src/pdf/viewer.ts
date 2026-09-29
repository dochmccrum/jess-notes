// PDF viewer (DESIGN §10.4, §7.7; lazy chunk with PDF.js). Bytes come through a custom
// PDFDataRangeTransport backed by the blob channel (local chunks first, then authenticated range
// fetches), so a 100 MB PDF opens without being loaded whole. Pages are virtualised: placeholders
// everywhere, canvases + text layers only near the viewport, and at most `maxLive` of them.
import { getDocument, GlobalWorkerOptions, PDFDataRangeTransport, TextLayer, type PDFDocumentProxy, type PDFPageProxy, type RenderTask } from 'pdfjs-dist/legacy/build/pdf.mjs'
import workerUrl from 'pdfjs-dist/legacy/build/pdf.worker.min.mjs?url'
import type { Backend } from '../backend/types'

import 'pdfjs-dist/web/pdf_viewer.css'
import './viewer.css'

GlobalWorkerOptions.workerSrc = workerUrl

export interface PdfSource {
  backend: Backend
  hash: string
  size: number
}

class BlobTransport extends PDFDataRangeTransport {
  failed: ((e: Error) => void) | null = null
  constructor(private src: PdfSource) {
    super(src.size, null)
  }
  requestDataRange(begin: number, end: number) {
    this.src.backend.blobRange(this.src.hash, begin, end).then(
      (b) => this.onDataRange(begin, b),
      (e) => this.failed?.(e instanceof Error ? e : new Error(String(e))),
    )
  }
}

/** Opens a PDF over the blob channel. */
export async function openPdf(src: PdfSource): Promise<PDFDocumentProxy> {
  const transport = new BlobTransport(src)
  const failed = new Promise<never>((_, reject) => (transport.failed = reject))
  const task = getDocument({
    range: transport,
    length: src.size,
    rangeChunkSize: 1 << 20,
    disableAutoFetch: true,
    disableStream: true,
    // PDF.js 5 never evals font code, and document JavaScript is never run (no scripting sandbox).
    enableXfa: false,
  })
  try {
    return await Promise.race([task.promise, failed])
  } catch (e) {
    void task.destroy()
    throw new Error(`This PDF isn't available offline (${(e as Error).message})`)
  }
}

export interface ViewerOptions {
  page?: number
  /** A scale factor, or `fit` (page width). */
  zoom?: number | 'fit'
  /** Fixed-height embedded viewer (no outline, compact chrome). */
  compact?: boolean
  onState?(s: ViewerState): void
  /** Called once, when the first page has been drawn. */
  onFirstPage?(): void
}

export interface ViewerState {
  page: number
  pages: number
  zoom: number
  fit: boolean
}

interface PageSlot {
  el: HTMLDivElement
  n: number
  w: number
  h: number
  live: boolean
  task: RenderTask | null
  text: TextLayer | null
}

const coarse = typeof matchMedia !== 'undefined' && matchMedia('(pointer: coarse)').matches

export class PdfViewer {
  readonly pages: number
  private slots: PageSlot[] = []
  private pagesEl: HTMLDivElement
  private io: IntersectionObserver
  private scale = 1
  private fit = true
  private base: { w: number; h: number } = { w: 612, h: 792 }
  private liveOrder: number[] = []
  private maxLive = coarse ? 6 : 12
  private current = 1
  private destroyed = false
  private texts = new Map<number, string>()
  private query = ''
  private matches: number[] = []
  private matchIndex = -1
  private scrollRaf = 0
  private ro: ResizeObserver

  constructor(
    private host: HTMLElement,
    private doc: PDFDocumentProxy,
    private opts: ViewerOptions = {},
  ) {
    this.pages = doc.numPages
    host.classList.add('jess-pdf-scroll')
    this.pagesEl = document.createElement('div')
    this.pagesEl.className = 'jess-pdf-pages'
    host.replaceChildren(this.pagesEl)
    this.io = new IntersectionObserver((es) => this.onIntersect(es), { root: host, rootMargin: '100% 0px' })
    this.ro = new ResizeObserver(() => {
      if (this.fit) this.applyZoom('fit', false)
    })
    host.addEventListener('scroll', this.onScroll, { passive: true })
    host.addEventListener('wheel', this.onWheel, { passive: false })
  }

  async init() {
    const p1 = await this.doc.getPage(1)
    const vp = p1.getViewport({ scale: 1 })
    this.base = { w: vp.width, h: vp.height }
    for (let n = 1; n <= this.pages; n++) {
      const el = document.createElement('div')
      el.className = 'jess-pdf-page'
      el.dataset.page = String(n)
      el.setAttribute('aria-label', `Page ${n}`)
      this.pagesEl.append(el)
      this.slots.push({ el, n, w: this.base.w, h: this.base.h, live: false, task: null, text: null })
    }
    const z = this.opts.zoom ?? 'fit'
    this.applyZoom(z, false)
    for (const s of this.slots) this.io.observe(s.el)
    this.ro.observe(this.host)
    if (this.opts.page && this.opts.page > 1) this.goto(this.opts.page)
    this.emit()
  }

  get state(): ViewerState {
    return { page: this.current, pages: this.pages, zoom: this.scale, fit: this.fit }
  }

  private emit() {
    this.opts.onState?.(this.state)
  }

  private sizeSlot(s: PageSlot) {
    s.el.style.width = `${Math.floor(s.w * this.scale)}px`
    s.el.style.height = `${Math.floor(s.h * this.scale)}px`
  }

  /** `fit` = page width; keeps the current page in view. */
  applyZoom(z: number | 'fit', keepPage = true) {
    const page = this.current
    this.fit = z === 'fit'
    const avail = Math.max(100, this.host.clientWidth - (this.opts.compact ? 8 : 32))
    this.scale = z === 'fit' ? avail / this.base.w : Math.min(6, Math.max(0.25, z))
    for (const s of this.slots) {
      this.sizeSlot(s)
      if (s.live) this.unrender(s)
    }
    this.liveOrder = []
    if (keepPage) this.goto(page)
    // Re-render what's visible at the new scale.
    for (const s of this.slots) {
      this.io.unobserve(s.el)
      this.io.observe(s.el)
    }
    this.emit()
  }

  zoomBy(f: number) {
    this.applyZoom(this.scale * f)
  }

  goto(page: number) {
    const n = Math.min(this.pages, Math.max(1, Math.round(page)))
    const s = this.slots[n - 1]
    if (!s) return
    this.host.scrollTop = s.el.offsetTop - 8
    this.current = n
    this.emit()
  }

  private onWheel = (e: WheelEvent) => {
    if (!(e.ctrlKey || e.metaKey)) return
    e.preventDefault()
    this.zoomBy(Math.exp(-e.deltaY / 300))
  }

  private onScroll = () => {
    if (this.scrollRaf) return
    this.scrollRaf = requestAnimationFrame(() => {
      this.scrollRaf = 0
      const mid = this.host.scrollTop + this.host.clientHeight / 3
      // Binary search over offsetTop (pages are in document order).
      let lo = 0
      let hi = this.slots.length - 1
      while (lo < hi) {
        const m = (lo + hi + 1) >> 1
        if (this.slots[m].el.offsetTop <= mid) lo = m
        else hi = m - 1
      }
      const p = lo + 1
      if (p !== this.current) {
        this.current = p
        this.emit()
      }
    })
  }

  private onIntersect(es: IntersectionObserverEntry[]) {
    for (const e of es) {
      const n = Number((e.target as HTMLElement).dataset.page)
      const s = this.slots[n - 1]
      if (e.isIntersecting && !s.live) void this.render(s)
    }
  }

  private async render(s: PageSlot) {
    if (this.destroyed) return
    s.live = true
    this.liveOrder.push(s.n)
    // Memory cap: drop the page farthest from the current one.
    while (this.liveOrder.length > this.maxLive) {
      let far = 0
      for (let i = 1; i < this.liveOrder.length; i++) if (Math.abs(this.liveOrder[i] - this.current) > Math.abs(this.liveOrder[far] - this.current)) far = i
      const [n] = this.liveOrder.splice(far, 1)
      this.unrender(this.slots[n - 1])
    }
    let page: PDFPageProxy
    try {
      page = await this.doc.getPage(s.n)
    } catch {
      s.live = false
      return
    }
    if (!s.live || this.destroyed) return
    const vp = page.getViewport({ scale: this.scale })
    const unscaled = page.getViewport({ scale: 1 })
    if (unscaled.width !== s.w || unscaled.height !== s.h) {
      s.w = unscaled.width
      s.h = unscaled.height
      this.sizeSlot(s)
    }
    const dpr = Math.min(window.devicePixelRatio || 1, coarse ? 2 : 3)
    const canvas = document.createElement('canvas')
    canvas.width = Math.floor(vp.width * dpr)
    canvas.height = Math.floor(vp.height * dpr)
    canvas.style.width = `${Math.floor(vp.width)}px`
    canvas.style.height = `${Math.floor(vp.height)}px`
    const textDiv = document.createElement('div')
    textDiv.className = 'textLayer'
    s.el.replaceChildren(canvas, textDiv)
    s.task = page.render({ canvas, canvasContext: canvas.getContext('2d')!, viewport: vp, transform: dpr !== 1 ? [dpr, 0, 0, dpr, 0, 0] : undefined })
    try {
      await s.task.promise
    } catch {
      return // cancelled
    }
    s.task = null
    if (!s.live) return
    if (this.opts.onFirstPage) {
      this.opts.onFirstPage()
      this.opts.onFirstPage = undefined
    }
    textDiv.style.setProperty('--scale-factor', String(this.scale))
    s.text = new TextLayer({ textContentSource: page.streamTextContent(), container: textDiv, viewport: vp })
    try {
      await s.text.render()
    } catch {
      /* cancelled */
    }
    if (this.query) this.highlight(s)
  }

  private unrender(s: PageSlot) {
    s.live = false
    s.task?.cancel()
    s.task = null
    s.text?.cancel()
    s.text = null
    for (const c of s.el.querySelectorAll('canvas')) {
      c.width = 0
      c.height = 0
    }
    s.el.replaceChildren()
    this.liveOrder = this.liveOrder.filter((n) => n !== s.n)
  }

  // ---------------------------------------------------------------- find

  private async pageText(n: number): Promise<string> {
    const hit = this.texts.get(n)
    if (hit !== undefined) return hit
    const page = await this.doc.getPage(n)
    const tc = await page.getTextContent()
    const t = tc.items.map((i) => ('str' in i ? i.str : '')).join(' ').toLowerCase()
    this.texts.set(n, t)
    return t
  }

  /** Finds `q` (case-insensitive); `dir` moves between pages with matches. */
  async find(q: string, dir: 1 | -1 = 1): Promise<{ index: number; total: number }> {
    const query = q.trim().toLowerCase()
    if (query !== this.query) {
      this.query = query
      this.matches = []
      this.matchIndex = -1
      if (query) for (let n = 1; n <= this.pages && !this.destroyed; n++) if ((await this.pageText(n)).includes(query)) this.matches.push(n)
      for (const s of this.slots) if (s.live) this.highlight(s)
    }
    if (!this.matches.length) return { index: 0, total: 0 }
    if (this.matchIndex < 0) {
      const after = this.matches.findIndex((p) => p >= this.current)
      this.matchIndex = dir === 1 ? Math.max(0, after) : (after <= 0 ? this.matches.length : after) - 1
    } else this.matchIndex = (this.matchIndex + dir + this.matches.length) % this.matches.length
    this.goto(this.matches[this.matchIndex])
    return { index: this.matchIndex + 1, total: this.matches.length }
  }

  private highlight(s: PageSlot) {
    for (const span of s.el.querySelectorAll<HTMLElement>('.textLayer span')) {
      span.classList.toggle('jess-found', !!this.query && (span.textContent ?? '').toLowerCase().includes(this.query))
    }
  }

  // ---------------------------------------------------------------- outline and thumbnails

  async outline(): Promise<{ title: string; page: number | null; depth: number }[]> {
    const out: { title: string; page: number | null; depth: number }[] = []
    const items = (await this.doc.getOutline()) ?? []
    const walk = async (xs: typeof items, depth: number) => {
      for (const it of xs) {
        let page: number | null = null
        try {
          const dest = typeof it.dest === 'string' ? await this.doc.getDestination(it.dest) : it.dest
          if (dest && dest[0]) page = (await this.doc.getPageIndex(dest[0])) + 1
        } catch {
          /* unresolvable destination */
        }
        out.push({ title: it.title, page, depth })
        if (it.items?.length && out.length < 2000) await walk(it.items, depth + 1)
      }
    }
    await walk(items, 0)
    return out
  }

  async renderThumb(n: number, canvas: HTMLCanvasElement, width = 120) {
    const page = await this.doc.getPage(n)
    const vp1 = page.getViewport({ scale: 1 })
    const vp = page.getViewport({ scale: width / vp1.width })
    canvas.width = Math.floor(vp.width)
    canvas.height = Math.floor(vp.height)
    await page.render({ canvas, canvasContext: canvas.getContext('2d')!, viewport: vp }).promise
  }

  destroy() {
    this.destroyed = true
    this.io.disconnect()
    this.ro.disconnect()
    this.host.removeEventListener('scroll', this.onScroll)
    this.host.removeEventListener('wheel', this.onWheel)
    for (const s of this.slots) this.unrender(s)
    this.host.replaceChildren()
  }
}
