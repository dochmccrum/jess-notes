// Editor construction (DESIGN §10): CodeMirror 6 + y-codemirror.next, created imperatively; no
// Svelte state is updated from CodeMirror transactions.

import { EditorState, Prec, type Extension } from '@codemirror/state'
import { EditorView, keymap, highlightSpecialChars, drawSelection, dropCursor, highlightActiveLine } from '@codemirror/view'
import { defaultKeymap, indentWithTab } from '@codemirror/commands'
import { markdownKeymap, markdownLanguage, pasteURLAsLink } from '@codemirror/lang-markdown'
import { Language, LanguageSupport, syntaxHighlighting, HighlightStyle, indentOnInput, bracketMatching } from '@codemirror/language'
import type { MarkdownParser } from '@lezer/markdown'
import { autocompletion, closeBrackets, closeBracketsKeymap, completionKeymap } from '@codemirror/autocomplete'
import { searchKeymap, highlightSelectionMatches } from '@codemirror/search'
import { tags as t } from '@lezer/highlight'
import * as Y from 'yjs'
import { yCollab, yUndoManagerKeymap } from 'y-codemirror.next'
import { obsidian, mdTags } from './syntax'
import { livePreview, linkAtPos, type LinkHandlers } from './livepreview'
import { wikilinkSource } from './autocomplete'
import type { EntryStore } from '../stores/entries'

const highlight = HighlightStyle.define([
  { tag: t.heading1, class: 'cm-h1' },
  { tag: t.heading2, class: 'cm-h2' },
  { tag: t.heading3, class: 'cm-h3' },
  { tag: [t.heading4, t.heading5, t.heading6], class: 'cm-h4' },
  { tag: t.strong, fontWeight: '700' },
  { tag: t.emphasis, fontStyle: 'italic' },
  { tag: t.strikethrough, textDecoration: 'line-through' },
  { tag: t.monospace, class: 'cm-inline-code' },
  { tag: t.link, class: 'cm-link' },
  { tag: t.url, class: 'cm-url' },
  { tag: t.quote, class: 'cm-quote' },
  { tag: [t.processingInstruction, t.meta], class: 'cm-meta' },
  { tag: mdTags.math, class: 'cm-math-src' },
  { tag: mdTags.highlight, class: 'cm-highlight' },
  { tag: mdTags.comment, class: 'cm-comment' },
])

/** `\r` is kept in the text (D11) but never shown. BOM and other specials stay visible. */
const SPECIALS = new RegExp(
  '[' +
    [[0, 8], [0x0b, 0x0c], [0x0e, 0x1f], [0x7f, 0x9f], [0xad, 0xad], [0x61c, 0x61c], [0x200b, 0x200b], [0x200e, 0x200f], [0x2028, 0x2029], [0x202d, 0x202e], [0x2066, 0x2067], [0x2069, 0x2069], [0xfff9, 0xfffc]]
      .map(([a, b]) => (a === b ? esc(a) : `${esc(a)}-${esc(b)}`))
      .join('') +
    ']',
  'g',
)
function esc(c: number) {
  return '\\u' + c.toString(16).padStart(4, '0')
}

/** Enter inserts `\r\n` when the note's dominant line ending is CRLF. */
function crlfNewline(dominantCRLF: boolean): Extension {
  if (!dominantCRLF) return []
  return keymap.of([
    {
      key: 'Enter',
      run: (view) => {
        view.dispatch(view.state.replaceSelection('\r\n'), { scrollIntoView: true, userEvent: 'input' })
        return true
      },
    },
  ])
}

export function dominantCRLF(text: string): boolean {
  const crlf = (text.match(/\r\n/g) ?? []).length
  const lf = (text.match(/\n/g) ?? []).length - crlf
  return crlf > lf
}

export interface EditorOptions {
  ydoc: Y.Doc
  store: EntryStore
  entryId: string
  links: LinkHandlers
  readOnly?: boolean
  /** Files pasted or dropped into the note. `pasted` = clipboard image data (no real name). */
  onFiles?(view: EditorView, files: File[], pasted: boolean): void
}

/**
 * Markdown (GFM + our Obsidian syntax) without `markdown()`'s `parseCode` step. That step nests
 * an HTML parser into HTML blocks and tags through `parseMixed`, which walks the whole tree after
 * every parse: a cost per keystroke that grows with the note (6 ms in a 1 MB note, DESIGN §23).
 * Raw HTML is shown as source anyway (§14), and we use no code-block languages. Kept from
 * `markdown()`: its keymap (list continuation) and pasting a URL over a selection as a link.
 */
let mdSupport: LanguageSupport | null = null
function markdownSupport(): LanguageSupport {
  mdSupport ??= new LanguageSupport(new Language(markdownLanguage.data, (markdownLanguage.parser as MarkdownParser).configure([obsidian]), [], 'markdown'), [
    pasteURLAsLink,
    Prec.high(keymap.of(markdownKeymap)),
  ])
  return mdSupport
}

export function createEditor(parent: HTMLElement, o: EditorOptions): EditorView {
  const ytext = o.ydoc.getText('t')
  const undo = new Y.UndoManager(ytext)
  const text = ytext.toString()
  const state = EditorState.create({
    doc: text,
    extensions: [
      EditorState.lineSeparator.of('\n'),
      highlightSpecialChars({ specialChars: SPECIALS }),
      drawSelection(),
      dropCursor(),
      highlightActiveLine(),
      EditorView.lineWrapping,
      indentOnInput(),
      bracketMatching(),
      closeBrackets(),
      highlightSelectionMatches(),
      markdownSupport(),
      syntaxHighlighting(highlight),
      livePreview(o.links),
      // No typing delay (default 100 ms): the link source is synchronous and a few ms at 30k names
      // (DESIGN §18: query → results <30 ms); outside `[[` it returns at once.
      autocompletion({ override: [wikilinkSource(o.store, () => o.entryId)], icons: false, interactionDelay: 0, activateOnTypingDelay: 0 }),
      crlfNewline(dominantCRLF(text)),
      keymap.of([...closeBracketsKeymap, ...completionKeymap, ...yUndoManagerKeymap, ...searchKeymap, ...defaultKeymap, indentWithTab]),
      yCollab(ytext, null, { undoManager: undo }),
      EditorState.readOnly.of(!!o.readOnly),
      EditorView.contentAttributes.of({ 'aria-label': 'Note editor', spellcheck: 'true', autocapitalize: 'sentences' }),
      EditorView.domEventHandlers({
        paste(e, view) {
          const files = [...(e.clipboardData?.files ?? [])]
          if (!files.length || !o.onFiles) return false
          e.preventDefault()
          // Screenshots arrive as "image.png": those get Obsidian's "Pasted image …" name.
          o.onFiles(view, files, files.every((f) => !f.name || /^image\.\w+$/.test(f.name)))
          return true
        },
        drop(e, view) {
          const files = [...(e.dataTransfer?.files ?? [])]
          if (!files.length || !o.onFiles) return false
          e.preventDefault()
          const pos = view.posAtCoords({ x: e.clientX, y: e.clientY })
          if (pos != null) view.dispatch({ selection: { anchor: pos } })
          o.onFiles(view, files, false)
          return true
        },
        mousedown(e, view) {
          if (!(e.metaKey || e.ctrlKey) || e.button !== 0) return false
          const pos = view.posAtCoords({ x: e.clientX, y: e.clientY })
          if (pos == null) return false
          const l = linkAtPos(view.state, pos)
          if (!l) return false
          e.preventDefault()
          o.links.open(l.target, l.markdown, l.subpath, e.shiftKey)
          return true
        },
      }),
    ],
  })
  return new EditorView({ state, parent })
}
