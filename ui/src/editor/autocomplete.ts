// `[[` autocomplete from the in-memory name index (DESIGN §11.4): fuzzy, <30 ms at 30k entries.
import type { CompletionContext, CompletionResult } from '@codemirror/autocomplete'
import type { EntryStore } from '../stores/entries'
import { EntryStore as ES } from '../stores/entries'
import { search } from '../lib/fuzzy'
import { displayName } from '../lib/names'

/** The link text for `id` from `src`: the bare name if it resolves uniquely, else the vault path. */
export function linkTextFor(store: EntryStore, id: string, src: string | null): string {
  const e = store.get(id)!
  const md = /\.md$/i.test(e.name)
  const bare = md ? e.name.slice(0, -3) : e.name
  const folder = src ? store.folderOf(src) : ''
  if (store.resolver.resolve(bare, 'wiki', folder)?.id === id) return bare
  const p = store.path(id) ?? e.name
  return md ? p.slice(0, -3) : p
}

export function wikilinkSource(store: EntryStore, src: () => string | null) {
  return (ctx: CompletionContext): CompletionResult | null => {
    const m = ctx.matchBefore(/!?\[\[[^\]\n|#]*$/)
    if (!m) return null
    const open = m.text.indexOf('[[') + 2
    const q = m.text.slice(open)
    const from = m.from + open
    const hits = q ? search(store.switcherCandidates(ES.wantsMedia(q)), q, 50) : [...store.entries.values()].filter((e) => e.kind === 'markdown' && !e.trashed && !e.purged).slice(0, 50).map((e) => ({ id: e.id }))
    const after = ctx.state.sliceDoc(ctx.pos, ctx.pos + 2)
    return {
      from,
      filter: false,
      options: hits.map((h) => {
        const e = store.get(h.id)!
        const text = linkTextFor(store, h.id, src())
        return {
          label: displayName(e.name),
          detail: store.folderOf(h.id) || undefined,
          type: e.kind === 'pdf' ? 'class' : e.kind === 'markdown' ? 'text' : 'constant',
          apply: after === ']]' ? text : text + ']]',
        }
      }),
    }
  }
}
