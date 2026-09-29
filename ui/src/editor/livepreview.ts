// Obsidian-style live preview (DESIGN §10.2). Inline marks are hidden except on lines holding a
// selection (a ViewPlugin over visible ranges: O(viewport)); display maths and own-line embeds are
// block widgets from a StateField updated incrementally (map + rescan touched blocks).

import { syntaxTree } from '@codemirror/language'
import { EditorState, RangeSetBuilder, StateEffect, StateField, type Extension, type Range } from '@codemirror/state'
import { Decoration, EditorView, ViewPlugin, WidgetType, type DecorationSet, type ViewUpdate } from '@codemirror/view'
import type { SyntaxNode } from '@lezer/common'
import { getRenderer, renderInto } from './renderers'

export interface LinkHandlers {
  resolve(target: string, markdown: boolean): string | null
  open(target: string, markdown: boolean, subpath: string | null, newPane: boolean): void
  embedRenderer?(target: string, resolved: string | null, subpath: string | null, display: string | null): { id: string; ctx: Record<string, unknown> } | null
}

function activeLines(state: EditorState): Set<number> {
  const s = new Set<number>()
  for (const r of state.selection.ranges) {
    const a = state.doc.lineAt(r.from).number
    const b = state.doc.lineAt(r.to).number
    for (let i = a; i <= b; i++) s.add(i)
  }
  return s
}

const hide = Decoration.replace({})

/** Dispatch when link targets may resolve differently (entries changed): links and embeds are
 * re-resolved without any document change. */
export const refreshLinks = StateEffect.define<null>()
const hasRefresh = (tr: { effects: readonly StateEffect<unknown>[] }) => tr.effects.some((e) => e.is(refreshLinks))

class LinkWidget extends WidgetType {
  constructor(
    readonly text: string,
    readonly target: string,
    readonly subpath: string | null,
    readonly resolved: boolean,
    readonly markdown: boolean,
  ) {
    super()
  }
  eq(o: LinkWidget) {
    return o.text === this.text && o.target === this.target && o.resolved === this.resolved && o.subpath === this.subpath
  }
  toDOM() {
    const a = document.createElement('span')
    a.className = `cm-link-widget ${this.resolved ? 'resolved' : 'unresolved'}`
    a.textContent = this.text
    a.dataset.target = this.target
    if (this.subpath) a.dataset.subpath = this.subpath
    if (this.markdown) a.dataset.markdown = '1'
    a.setAttribute('role', 'link')
    return a
  }
  ignoreEvent() {
    return false
  }
}

class RenderWidget extends WidgetType {
  private teardown: (() => void) | null = null
  constructor(
    readonly renderer: string,
    readonly key: string,
    readonly ctx: Record<string, unknown>,
    readonly block: boolean,
  ) {
    super()
  }
  eq(o: RenderWidget) {
    return o.renderer === this.renderer && o.key === this.key
  }
  toDOM() {
    const el = document.createElement(this.block ? 'div' : 'span')
    el.className = `cm-render cm-render-${this.renderer}${this.block ? ' cm-render-block' : ''}`
    this.teardown = renderInto(this.renderer, this.ctx as never, el)
    return el
  }
  destroy() {
    this.teardown?.()
  }
  ignoreEvent() {
    return false
  }
  get estimatedHeight() {
    if (!this.block) return -1
    const est = getRenderer(this.renderer)?.estimateSize?.(this.ctx as never)
    return est ? est.height : 40
  }
}

function wikiParts(state: EditorState, node: SyntaxNode) {
  let target = ''
  let alias: string | null = null
  let subpath: string | null = null
  for (let c = node.firstChild; c; c = c.nextSibling) {
    if (c.name === 'WikiTarget') target = state.sliceDoc(c.from, c.to)
    else if (c.name === 'WikiAlias') alias = state.sliceDoc(c.from, c.to)
    else if (c.name === 'WikiSubpath') subpath = state.sliceDoc(c.from, c.to)
  }
  return { target, alias, subpath }
}

function mdLinkParts(state: EditorState, node: SyntaxNode) {
  const url = node.getChild('URL')
  let raw = url ? state.sliceDoc(url.from, url.to) : ''
  if (raw.startsWith('<') && raw.endsWith('>')) raw = raw.slice(1, -1)
  const hash = raw.indexOf('#')
  let target = hash >= 0 ? raw.slice(0, hash) : raw
  try {
    target = decodeURIComponent(target)
  } catch {
    /* keep raw */
  }
  const marks = node.getChildren('LinkMark')
  const text = marks.length >= 2 ? state.sliceDoc(marks[0].to, marks[1].from) : ''
  return { target, subpath: hash >= 0 ? raw.slice(hash) : null, text, url }
}

/** The node is the whole of its paragraph (then it's a block widget, not an inline one). */
function ownsParagraph(state: EditorState, n: SyntaxNode): boolean {
  const p = n.parent
  return !!p && p.name === 'Paragraph' && p.firstChild?.from === n.from && !n.nextSibling && state.sliceDoc(p.from, p.to).trim() === state.sliceDoc(n.from, n.to)
}

function isExternal(t: string) {
  return /^[a-zA-Z][a-zA-Z0-9+.-]+:/.test(t)
}

function inlineDecos(view: EditorView, h: LinkHandlers): DecorationSet {
  const { state } = view
  const active = activeLines(state)
  const out: Range<Decoration>[] = []
  const lineOf = (pos: number) => state.doc.lineAt(pos).number
  for (const { from, to } of view.visibleRanges) {
    syntaxTree(state).iterate({
      from,
      to,
      enter: (n) => {
        const onActive = active.has(lineOf(n.from)) || active.has(lineOf(n.to))
        switch (n.name) {
          case 'FencedCode':
          case 'CodeBlock':
          case 'BlockMath':
          case 'CommentBlock':
            return false
          case 'HeaderMark':
            if (!onActive && n.node.parent?.name.startsWith('ATXHeading')) {
              const end = state.sliceDoc(n.to, n.to + 1) === ' ' ? n.to + 1 : n.to
              out.push(hide.range(n.from, end))
            }
            return
          case 'EmphasisMark':
          case 'StrikethroughMark':
          case 'HighlightMark':
            if (!onActive) out.push(hide.range(n.from, n.to))
            return
          case 'CodeMark':
            if (!onActive && n.node.parent?.name === 'InlineCode') out.push(hide.range(n.from, n.to))
            return
          case 'Tag':
            out.push(Decoration.mark({ class: 'cm-tag', attributes: { 'data-tag': state.sliceDoc(n.from + 1, n.to) } }).range(n.from, n.to))
            return false
          case 'Comment':
            out.push(Decoration.mark({ class: 'cm-comment' }).range(n.from, n.to))
            return false
          case 'WikiLink': {
            const { target, alias, subpath } = wikiParts(state, n.node)
            const resolved = !target || !!h.resolve(target, false)
            if (onActive) {
              out.push(Decoration.mark({ class: `cm-wikilink ${resolved ? 'resolved' : 'unresolved'}` }).range(n.from, n.to))
            } else {
              const text = alias ?? (target + (subpath ? ` › ${subpath.slice(1)}` : '') || subpath?.slice(1) || '')
              out.push(Decoration.replace({ widget: new LinkWidget(text, target, subpath, resolved, false) }).range(n.from, n.to))
            }
            return false
          }
          case 'Embed': {
            if (onActive) {
              out.push(Decoration.mark({ class: 'cm-embed-src' }).range(n.from, n.to))
              return false
            }
            if (ownsParagraph(state, n.node)) return false // the block field renders it
            const { target, alias, subpath } = wikiParts(state, n.node)
            const r = h.embedRenderer?.(target, h.resolve(target, false), subpath, alias) ?? { id: 'transclusion', ctx: { source: target, subpath } }
            out.push(Decoration.replace({ widget: new RenderWidget(r.id, JSON.stringify(r.ctx), r.ctx, false) }).range(n.from, n.to))
            return false
          }
          case 'Link': {
            const { target, subpath, text, url } = mdLinkParts(state, n.node)
            if (!url) return
            const ext = isExternal(target)
            const resolved = ext || !!h.resolve(target, true)
            if (onActive) {
              out.push(Decoration.mark({ class: `cm-md-link ${resolved ? 'resolved' : 'unresolved'}` }).range(n.from, n.to))
            } else {
              out.push(Decoration.replace({ widget: new LinkWidget(text || target, ext ? state.sliceDoc(url.from, url.to) : target, subpath, resolved, true) }).range(n.from, n.to))
            }
            return false
          }
          case 'Image': {
            if (onActive) return false
            if (ownsParagraph(state, n.node)) return false
            const { target, subpath, text } = mdLinkParts(state, n.node)
            const r = h.embedRenderer?.(target, isExternal(target) ? null : h.resolve(target, true), subpath, text) ?? null
            if (r) out.push(Decoration.replace({ widget: new RenderWidget(r.id, JSON.stringify(r.ctx), r.ctx, false) }).range(n.from, n.to))
            return false
          }
          case 'InlineMath': {
            if (onActive) {
              out.push(Decoration.mark({ class: 'cm-math-src' }).range(n.from, n.to))
              return false
            }
            const src = state.sliceDoc(n.from, n.to)
            const display = src.startsWith('$$')
            const tex = display ? src.slice(2, -2) : src.slice(1, -1)
            out.push(Decoration.replace({ widget: new RenderWidget('math', (display ? 'D' : 'I') + tex, { source: tex, display }, false) }).range(n.from, n.to))
            return false
          }
        }
      },
    })
  }
  out.sort((a, b) => a.from - b.from || a.value.startSide - b.value.startSide)
  const b = new RangeSetBuilder<Decoration>()
  let last = -1
  for (const r of out) {
    if (r.from < last) continue // overlapping (nested) decorations: keep the outer one
    b.add(r.from, r.to, r.value)
    last = r.to
  }
  return b.finish()
}

function blockDecosIn(state: EditorState, from: number, to: number, h: LinkHandlers): Range<Decoration>[] {
  const out: Range<Decoration>[] = []
  const sel = state.selection.ranges
  const touches = (a: number, b: number) => sel.some((r) => r.from <= b && r.to >= a)
  const top = syntaxTree(state).topNode
  for (let c = top.childAfter(Math.max(0, from - 1)) ?? top.firstChild; c && c.from <= to; c = c.nextSibling) {
    if (c.to < from) continue
    if (c.name === 'BlockMath') {
      if (touches(c.from, c.to)) continue
      const src = state.sliceDoc(c.from, c.to).trim()
      const tex = src.replace(/^\$\$/, '').replace(/\$\$$/, '')
      out.push(Decoration.replace({ widget: new RenderWidget('math', 'D' + tex, { source: tex, display: true }, true), block: true }).range(c.from, c.to))
    } else if (c.name === 'Paragraph') {
      const only = c.firstChild
      if (!only || (only.name !== 'Embed' && only.name !== 'Image') || !ownsParagraph(state, only)) continue
      if (touches(c.from, c.to)) continue
      let r: { id: string; ctx: Record<string, unknown> } | null
      if (only.name === 'Embed') {
        const { target, alias, subpath } = wikiParts(state, only)
        r = h.embedRenderer?.(target, h.resolve(target, false), subpath, alias) ?? { id: 'transclusion', ctx: { source: target, subpath } }
      } else {
        const { target, subpath, text } = mdLinkParts(state, only)
        r = h.embedRenderer?.(target, isExternal(target) ? null : h.resolve(target, true), subpath, text) ?? null
      }
      if (r) out.push(Decoration.replace({ widget: new RenderWidget(r.id, JSON.stringify(r.ctx), r.ctx, true), block: true }).range(c.from, c.to))
    }
  }
  return out
}

function blockField(h: LinkHandlers) {
  return StateField.define<DecorationSet>({
    create: (state) => Decoration.set(blockDecosIn(state, 0, state.doc.length, h), true),
    update(decos, tr) {
      if (hasRefresh(tr)) return Decoration.set(blockDecosIn(tr.state, 0, tr.state.doc.length, h), true)
      if (!tr.docChanged && !tr.selection && syntaxTree(tr.startState) === syntaxTree(tr.state)) return decos
      // Incremental: map, then rescan only blocks touched by the change or the selection.
      let lo = Infinity
      let hi = -1
      const mapped = decos.map(tr.changes)
      tr.changes.iterChangedRanges((_fa, _ta, fb, tb) => {
        lo = Math.min(lo, fb)
        hi = Math.max(hi, tb)
      })
      for (const r of [...tr.startState.selection.ranges.map((r) => ({ from: tr.changes.mapPos(r.from), to: tr.changes.mapPos(r.to) })), ...tr.state.selection.ranges]) {
        lo = Math.min(lo, r.from)
        hi = Math.max(hi, r.to)
      }
      if (syntaxTree(tr.startState) !== syntaxTree(tr.state) && !tr.docChanged) {
        // The parser progressed (large doc): rescan everything once.
        return Decoration.set(blockDecosIn(tr.state, 0, tr.state.doc.length, h), true)
      }
      if (hi < 0) return mapped
      const top = syntaxTree(tr.state).topNode
      const a = top.childBefore(lo)?.from ?? 0
      const b = top.childAfter(hi)?.to ?? tr.state.doc.length
      return mapped.update({ filter: (f, t) => t < a || f > b, add: blockDecosIn(tr.state, a, b, h), sort: true })
    },
    provide: (f) => EditorView.decorations.from(f),
  })
}

export function livePreview(h: LinkHandlers): Extension {
  const plugin = ViewPlugin.fromClass(
    class {
      decorations: DecorationSet
      constructor(view: EditorView) {
        this.decorations = inlineDecos(view, h)
      }
      update(u: ViewUpdate) {
        if (u.docChanged || u.viewportChanged || u.selectionSet || syntaxTree(u.startState) !== syntaxTree(u.state) || u.transactions.some(hasRefresh)) this.decorations = inlineDecos(u.view, h)
      }
    },
    {
      decorations: (v) => v.decorations,
      eventHandlers: {
        mousedown(e) {
          const t = (e.target as HTMLElement).closest('.cm-link-widget') as HTMLElement | null
          if (!t || e.button !== 0) return false
          e.preventDefault()
          h.open(t.dataset.target ?? '', !!t.dataset.markdown, t.dataset.subpath ?? null, e.metaKey || e.ctrlKey)
          return true
        },
      },
    },
  )
  return [plugin, blockField(h)]
}

/** Mod-click on raw link text (the cursor's line) follows the link. */
export function linkAtPos(state: EditorState, pos: number): { target: string; markdown: boolean; subpath: string | null } | null {
  let n: SyntaxNode | null = syntaxTree(state).resolveInner(pos, 1)
  while (n && !['WikiLink', 'Embed', 'Link', 'Image'].includes(n.name)) n = n.parent
  if (!n) return null
  if (n.name === 'WikiLink' || n.name === 'Embed') {
    const { target, subpath } = wikiParts(state, n)
    return { target, markdown: false, subpath }
  }
  const { target, subpath } = mdLinkParts(state, n)
  return { target, markdown: true, subpath }
}
