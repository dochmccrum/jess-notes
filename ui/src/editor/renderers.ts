// Renderer registry (DESIGN §10.3): maths, images, PDF embeds and the transclusion placeholder
// are clients; Mermaid, Excalidraw, recipes plug in the same way later. Each renderer's code is a
// separate lazy chunk; until it loads, a placeholder sized by `estimateSize` is shown.

export interface RenderCtx {
  source: string // e.g. the TeX source, or the embed target
  display: boolean
  target?: string // resolved entry id (embeds)
  subpath?: string
  alt?: string
  size?: [number, number | null] | null
}

export interface RendererImpl {
  render(ctx: RenderCtx, el: HTMLElement): void | (() => void)
}

export interface Renderer {
  id: string
  display: 'inline' | 'block'
  load(): Promise<RendererImpl>
  estimateSize?(ctx: RenderCtx): { width?: number; height: number } | null
  cacheKey?(ctx: RenderCtx): string
}

const registry = new Map<string, Renderer>()
const loaded = new Map<string, RendererImpl>()
const loading = new Map<string, Promise<RendererImpl>>()

export function registerRenderer(r: Renderer) {
  registry.set(r.id, r)
}

export function getRenderer(id: string) {
  return registry.get(id)
}

/** Renders into `el` now if loaded, else shows a placeholder and renders when the chunk arrives. */
export function renderInto(id: string, ctx: RenderCtx, el: HTMLElement): () => void {
  const r = registry.get(id)
  let teardown: void | (() => void)
  let dead = false
  const go = (impl: RendererImpl) => {
    if (dead) return
    el.replaceChildren()
    el.classList.remove('jess-placeholder')
    try {
      teardown = impl.render(ctx, el)
    } catch (e) {
      // A renderer that throws never blanks the line: show the source with an error badge.
      el.replaceChildren()
      const code = document.createElement('code')
      code.textContent = ctx.source
      const badge = document.createElement('span')
      badge.className = 'jess-render-error'
      badge.title = String(e)
      badge.textContent = '⚠'
      el.append(code, badge)
    }
  }
  if (!r) {
    el.textContent = ctx.source
    return () => {}
  }
  const hit = loaded.get(id)
  if (hit) go(hit)
  else {
    el.classList.add('jess-placeholder')
    const est = r.estimateSize?.(ctx)
    if (est) {
      el.style.minHeight = `${est.height}px`
      if (est.width) el.style.width = `${est.width}px`
    }
    el.textContent = r.display === 'block' ? '' : ctx.source
    let p = loading.get(id)
    if (!p) {
      p = r.load().then((impl) => {
        loaded.set(id, impl)
        return impl
      })
      loading.set(id, p)
    }
    void p.then(go, () => {})
  }
  return () => {
    dead = true
    if (typeof teardown === 'function') teardown()
  }
}

// ---------------------------------------------------------------- built-in renderers

registerRenderer({
  id: 'math',
  display: 'inline',
  load: () => import('./math-render').then((m) => m.impl),
  estimateSize: (c) => (c.display ? { height: 40 } : null),
  cacheKey: (c) => (c.display ? 'D' : 'I') + c.source,
})

registerRenderer({
  id: 'transclusion',
  display: 'block',
  load: async () => ({
    render(ctx, el) {
      el.className = 'jess-transclusion'
      el.textContent = `↪ ${ctx.source}${ctx.subpath ?? ''}`
      el.title = 'Transclusion is not rendered yet'
    },
  }),
})
