// Lezer markdown extensions for Obsidian syntax (DESIGN §10.1, §10.5). Display-only: the Rust
// core is the authority for links/tags; both are tested against core/fixtures/*.json.

import type { MarkdownConfig, InlineContext, BlockContext, Line, Element } from '@lezer/markdown'
import { tags as t, Tag } from '@lezer/highlight'

export const mdTags = {
  wikiLink: Tag.define(t.link),
  embed: Tag.define(t.link),
  wikiMark: Tag.define(t.processingInstruction),
  math: Tag.define(t.string),
  mathMark: Tag.define(t.processingInstruction),
  tag: Tag.define(t.labelName),
  comment: Tag.define(t.comment),
  highlight: Tag.define(t.content),
}

const SPACE = (c: number) => c === 32 || c === 9 || c === 10 || c === 13
const DIGIT = (c: number) => c >= 48 && c <= 57

function wikiEnd(cx: InlineContext, from: number): number {
  // `]]` on the same line, no nested `[[`.
  for (let i = from; i < cx.end - 1; i++) {
    const c = cx.char(i)
    if (c === 10) return -1
    if (c === 91 && cx.char(i + 1) === 91) return -1
    if (c === 93 && cx.char(i + 1) === 93) return i
  }
  return -1
}

function parseWiki(cx: InlineContext, pos: number, embed: boolean): number {
  const open = embed ? pos + 1 : pos
  if (cx.char(open) !== 91 || cx.char(open + 1) !== 91) return -1
  const inner = open + 2
  const close = wikiEnd(cx, inner)
  if (close < 0) return -1
  const text = cx.slice(inner, close)
  if (!text.trim()) return -1
  const children: Element[] = [cx.elt('WikiMark', pos, inner)]
  const pipe = text.indexOf('|')
  let left = pipe >= 0 ? text.slice(0, pipe) : text
  if (left.endsWith('\\')) left = left.slice(0, -1)
  const hash = left.indexOf('#')
  const target = hash >= 0 ? left.slice(0, hash) : left
  const lead = target.length - target.trimStart().length
  const tt = target.trim()
  if (tt) children.push(cx.elt('WikiTarget', inner + lead, inner + lead + tt.length))
  if (hash >= 0) children.push(cx.elt('WikiSubpath', inner + hash, inner + left.length))
  if (pipe >= 0) {
    children.push(cx.elt('WikiMark', inner + pipe, inner + pipe + 1))
    children.push(cx.elt('WikiAlias', inner + pipe + 1, close))
  }
  children.push(cx.elt('WikiMark', close, close + 2))
  return cx.addElement(cx.elt(embed ? 'Embed' : 'WikiLink', pos, close + 2, children))
}

function inlineMathEnd(cx: InlineContext, pos: number): number {
  const n = cx.char(pos + 1)
  if (n < 0 || SPACE(n) || n === 36) return -1
  for (let j = pos + 1; j < cx.end; j++) {
    const c = cx.char(j)
    if (c === 92) {
      j++
      continue
    }
    if (c === 10) {
      // No blank line inside.
      let k = j + 1
      while (k < cx.end && (cx.char(k) === 32 || cx.char(k) === 9 || cx.char(k) === 13)) k++
      if (k >= cx.end || cx.char(k) === 10) return -1
    }
    if (c === 36 && j > pos + 1) {
      const prev = cx.char(j - 1)
      const next = cx.char(j + 1)
      if (!SPACE(prev) && !DIGIT(next)) return j + 1
    }
  }
  return -1
}

export const obsidian: MarkdownConfig = {
  defineNodes: [
    { name: 'WikiLink', style: mdTags.wikiLink },
    { name: 'Embed', style: mdTags.embed },
    { name: 'WikiMark', style: mdTags.wikiMark },
    { name: 'WikiTarget' },
    { name: 'WikiSubpath' },
    { name: 'WikiAlias' },
    { name: 'InlineMath', style: mdTags.math },
    { name: 'BlockMath', block: true, style: mdTags.math },
    { name: 'MathMark', style: mdTags.mathMark },
    { name: 'Tag', style: mdTags.tag },
    { name: 'Comment', style: mdTags.comment },
    { name: 'CommentBlock', block: true, style: mdTags.comment },
    { name: 'Highlight', style: mdTags.highlight },
    { name: 'HighlightMark', style: mdTags.wikiMark },
  ],
  parseInline: [
    {
      name: 'Embed',
      before: 'Image',
      parse(cx, next, pos) {
        return next === 33 && cx.char(pos + 1) === 91 && cx.char(pos + 2) === 91 ? parseWiki(cx, pos, true) : -1
      },
    },
    {
      name: 'WikiLink',
      before: 'Link',
      parse(cx, next, pos) {
        if (next !== 91 || cx.char(pos + 1) !== 91) return -1
        const r = parseWiki(cx, pos, false)
        return r
      },
    },
    {
      name: 'Comment',
      before: 'Emphasis',
      parse(cx, next, pos) {
        if (next !== 37 || cx.char(pos + 1) !== 37) return -1
        const close = cx.slice(pos + 2, cx.end).indexOf('%%')
        if (close < 0) return -1
        return cx.addElement(cx.elt('Comment', pos, pos + 2 + close + 2))
      },
    },
    {
      name: 'InlineMath',
      after: 'Escape',
      before: 'Emphasis',
      parse(cx, next, pos) {
        if (next !== 36) return -1
        if (cx.char(pos + 1) === 36) {
          // `$$…$$` inside a paragraph: display maths.
          const rest = cx.slice(pos + 2, cx.end)
          const k = rest.indexOf('$$')
          if (k < 0) return -1
          if (/\n[ \t\r]*\n/.test(rest.slice(0, k))) return -1
          const end = pos + 2 + k + 2
          return cx.addElement(cx.elt('InlineMath', pos, end, [cx.elt('MathMark', pos, pos + 2), cx.elt('MathMark', end - 2, end)]))
        }
        const end = inlineMathEnd(cx, pos)
        if (end < 0) return -1
        return cx.addElement(cx.elt('InlineMath', pos, end, [cx.elt('MathMark', pos, pos + 1), cx.elt('MathMark', end - 1, end)]))
      },
    },
    {
      name: 'Tag',
      before: 'Emphasis',
      parse(cx, next, pos) {
        if (next !== 35) return -1
        const prev = pos > cx.offset ? cx.char(pos - 1) : 32
        if (pos > cx.offset && !SPACE(prev)) return -1
        const m = /^[\p{L}\p{N}_/-]+/u.exec(cx.slice(pos + 1, cx.end))
        if (!m || !/[^0-9]/.test(m[0])) return -1
        return cx.addElement(cx.elt('Tag', pos, pos + 1 + m[0].length))
      },
    },
    {
      name: 'Highlight',
      before: 'Emphasis',
      parse(cx, next, pos) {
        if (next !== 61 || cx.char(pos + 1) !== 61 || SPACE(cx.char(pos + 2))) return -1
        const k = cx.slice(pos + 2, cx.end).indexOf('==')
        if (k <= 0) return -1
        const end = pos + 2 + k + 2
        return cx.addElement(cx.elt('Highlight', pos, end, [cx.elt('HighlightMark', pos, pos + 2), cx.elt('HighlightMark', end - 2, end)]))
      },
    },
  ],
  parseBlock: [
    {
      name: 'BlockMath',
      before: 'FencedCode',
      parse(cx: BlockContext, line: Line) {
        if (line.indent > 3 || !line.text.slice(line.pos).startsWith('$$')) return false
        const start = cx.lineStart + line.pos
        const rest = line.text.slice(line.pos + 2)
        const same = rest.indexOf('$$')
        if (same >= 0) {
          const end = start + 2 + same + 2
          cx.addElement(cx.elt('BlockMath', start, cx.lineStart + line.text.length, [cx.elt('MathMark', start, start + 2), cx.elt('MathMark', end - 2, end)]))
          cx.nextLine()
          return true
        }
        // Look ahead for the closing line; unterminated `$$` stays plain text (§10.5).
        const doc = (cx as unknown as { input: { read(a: number, b: number): string; length: number } }).input
        const after = cx.lineStart + line.text.length
        const tail = doc.read(after, Math.min(doc.length, after + 200000))
        const k = tail.indexOf('$$')
        if (k < 0) return false
        const marks: Element[] = [cx.elt('MathMark', start, start + 2)]
        while (cx.nextLine()) {
          const lineEnd = cx.lineStart + line.text.length
          const idx = line.text.indexOf('$$')
          if (idx >= 0) {
            marks.push(cx.elt('MathMark', cx.lineStart + idx, cx.lineStart + idx + 2))
            cx.addElement(cx.elt('BlockMath', start, lineEnd, marks))
            cx.nextLine()
            return true
          }
        }
        cx.addElement(cx.elt('BlockMath', start, cx.lineStart, marks))
        return true
      },
      endLeaf(_cx, line) {
        return line.text.slice(line.pos).startsWith('$$')
      },
    },
    {
      name: 'CommentBlock',
      before: 'FencedCode',
      parse(cx: BlockContext, line: Line) {
        const txt = line.text.slice(line.pos)
        if (line.indent > 3 || !txt.startsWith('%%') || txt.slice(2).includes('%%')) return false
        const doc = (cx as unknown as { input: { read(a: number, b: number): string; length: number } }).input
        const after = cx.lineStart + line.text.length
        if (doc.read(after, Math.min(doc.length, after + 500000)).indexOf('%%') < 0) return false
        const start = cx.lineStart + line.pos
        while (cx.nextLine()) {
          if (line.text.includes('%%')) {
            const end = cx.lineStart + line.text.length
            cx.addElement(cx.elt('CommentBlock', start, end))
            cx.nextLine()
            return true
          }
        }
        cx.addElement(cx.elt('CommentBlock', start, cx.lineStart))
        return true
      },
    },
  ],
}
