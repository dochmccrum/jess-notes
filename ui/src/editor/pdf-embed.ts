// Placeholder until the PDF embed renderer lands (thumbnail → live viewer).
import type { RendererImpl } from './renderers'
import { env } from './renderers'
import type { PdfCtx } from './embeds'

export const impl: RendererImpl = {
  render(ctx0, el) {
    const ctx = ctx0 as unknown as PdfCtx
    const a = document.createElement('div')
    a.className = 'jess-pdf-embed-link'
    a.textContent = `📄 ${ctx.source}${ctx.page > 1 ? ` (page ${ctx.page})` : ''}`
    a.onclick = () => ctx.entryId && env?.openEntry(ctx.entryId, `#page=${ctx.page}`)
    el.append(a)
  },
}
