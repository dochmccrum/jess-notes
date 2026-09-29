// KaTeX maths renderer (lazy chunk; DESIGN §10.5). Output is cached by source string.
import katex from 'katex'
import 'katex/dist/katex.min.css'
import type { RendererImpl } from './renderers'

const cache = new Map<string, HTMLElement>()
const MAX = 2000

function renderToTemplate(src: string, display: boolean): HTMLElement {
  const key = (display ? 'D' : 'I') + src
  const hit = cache.get(key)
  if (hit) {
    cache.delete(key)
    cache.set(key, hit)
    return hit
  }
  const el = document.createElement(display ? 'div' : 'span')
  try {
    katex.render(src, el, { displayMode: display, throwOnError: true, trust: false, strict: 'ignore', maxSize: 50, maxExpand: 1000, output: 'htmlAndMathml' })
  } catch (e) {
    // Invalid TeX: the source with a subtle inline error, never a crash or a blank line.
    el.replaceChildren()
    el.className = 'jess-math-error'
    el.textContent = src
    el.title = e instanceof Error ? e.message : String(e)
  }
  cache.set(key, el)
  if (cache.size > MAX) cache.delete(cache.keys().next().value!)
  return el
}

export const impl: RendererImpl = {
  render(ctx, el) {
    const src = ctx.source
    el.append(renderToTemplate(src, ctx.display).cloneNode(true))
  },
}
