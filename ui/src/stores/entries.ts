// Flat, fine-grained metadata store (DESIGN §11.3). Never holds note bodies or blobs.

import type { EntryMeta } from '../lib/types'
import { isLinkable, VAULT_SETTINGS_ID } from '../lib/types'
import { Resolver } from '../lib/resolver'
import { charMask, type Candidate } from '../lib/fuzzy'
import { isImageName } from '../lib/names'

const collator = new Intl.Collator(undefined, { numeric: true, sensitivity: 'base' })

export type Listener = (changed: string[]) => void

export class EntryStore {
  readonly entries = new Map<string, EntryMeta>()
  private kids = new Map<string | null, string[]>()
  private kidsDirty = new Set<string | null>()
  private visibleDocs = new Map<string, number>()
  private listeners = new Set<Listener>()
  readonly resolver = new Resolver()
  version = 0
  showAllAttachments = false
  private sortKeys = new Map<string, string>()
  private switcher: Candidate[] | null = null

  subscribe(l: Listener): () => void {
    this.listeners.add(l)
    return () => this.listeners.delete(l)
  }

  load(list: EntryMeta[]) {
    this.entries.clear()
    this.kids.clear()
    this.resolver.clear()
    for (const e of list) this.entries.set(e.id, e)
    for (const e of list) this.index(e)
    this.bump(list.map((e) => e.id), true)
  }

  /** Applies changed entries (`{id, deleted: true}` removes). */
  apply(list: (EntryMeta | { id: string; deleted: true })[]) {
    const changed: string[] = []
    const parents = new Set<string | null>()
    for (const x of list) {
      const old = this.entries.get(x.id)
      if (old) parents.add(old.parent)
      if ('deleted' in x) {
        this.entries.delete(x.id)
        this.resolver.remove(x.id)
      } else {
        this.entries.set(x.id, x)
        parents.add(x.parent)
      }
      changed.push(x.id)
    }
    for (const id of changed) {
      const e = this.entries.get(id)
      if (e) this.index(e)
    }
    for (const p of parents) this.kidsDirty.add(p)
    // Folder moves/renames change descendants' paths.
    const folders = changed.filter((id) => this.entries.get(id)?.kind === 'folder')
    if (folders.length) for (const d of this.descendantsOf(folders)) this.index(this.entries.get(d)!)
    this.bump(changed, false)
  }

  private index(e: EntryMeta) {
    this.sortKeys.set(e.id, e.name)
    if (isLinkable(e)) {
      const p = this.path(e.id)
      if (p !== null) this.resolver.insert(e.id, p)
    } else this.resolver.remove(e.id)
  }

  private bump(changed: string[], all: boolean) {
    this.version++
    this.visibleDocs.clear()
    this.switcher = null
    if (all) {
      this.kids.clear()
      this.kidsDirty.clear()
    }
    for (const l of this.listeners) l(changed)
  }

  get(id: string) {
    return this.entries.get(id)
  }

  path(id: string): string | null {
    const parts: string[] = []
    let cur: string | null = id
    let guard = 0
    while (cur) {
      const e = this.entries.get(cur)
      if (!e) return null
      parts.push(e.name)
      cur = e.parent
      if (++guard > 1000) return null
    }
    return parts.reverse().join('/')
  }

  folderOf(id: string): string {
    const p = this.entries.get(id)?.parent
    return p ? (this.path(p) ?? '') : ''
  }

  ancestors(id: string): string[] {
    const out: string[] = []
    let cur = this.entries.get(id)?.parent ?? null
    while (cur) {
      out.push(cur)
      cur = this.entries.get(cur)?.parent ?? null
    }
    return out
  }

  descendants(id: string): string[] {
    return this.descendantsOf([id])
  }

  descendantsOf(ids: string[]): string[] {
    // One pass to index children, then a walk: O(n) however deep or wide the subtree is.
    const byParent = new Map<string, string[]>()
    for (const e of this.entries.values()) {
      if (e.purged || !e.parent) continue
      const v = byParent.get(e.parent)
      if (v) v.push(e.id)
      else byParent.set(e.parent, [e.id])
    }
    const out: string[] = []
    const stack = [...ids]
    const seen = new Set(ids)
    while (stack.length) {
      for (const c of byParent.get(stack.pop()!) ?? []) {
        if (seen.has(c)) continue
        seen.add(c)
        out.push(c)
        stack.push(c)
      }
    }
    return out
  }

  /** Is this entry shown in the file tree (§3.1)? */
  inTree(e: EntryMeta): boolean {
    if (e.trashed || e.purged || e.id === VAULT_SETTINGS_ID || e.kind === 'vault') return false
    if (e.kind === 'markdown') return true
    if (e.kind === 'pdf') return e.visible || this.showAllAttachments
    if (e.kind === 'folder') return this.folderShown(e.id)
    return this.showAllAttachments
  }

  /** A folder is shown if it's empty or its subtree has a visible document. */
  folderShown(id: string): boolean {
    const kids = this.liveChildren(id)
    if (kids.length === 0) return true
    return this.countVisible(id) > 0
  }

  private countVisible(id: string): number {
    const hit = this.visibleDocs.get(id)
    if (hit !== undefined) return hit
    let n = 0
    for (const c of this.liveChildren(id)) {
      const e = this.entries.get(c)!
      if (e.kind === 'folder') n += this.liveChildren(c).length === 0 ? 1 : this.countVisible(c)
      else if (e.kind === 'markdown' || (e.kind === 'pdf' && e.visible) || this.showAllAttachments) n++
    }
    this.visibleDocs.set(id, n)
    return n
  }

  /** Live children sorted: folders first, natural order (cached per folder until it changes). */
  liveChildren(parent: string | null): string[] {
    if (this.kids.has(parent) && !this.kidsDirty.has(parent)) return this.kids.get(parent)!
    if (this.kids.size === 0 && this.entries.size > 0) this.rebuildAllKids()
    else {
      const v: string[] = []
      for (const e of this.entries.values()) if (e.parent === parent && !e.trashed && !e.purged && e.kind !== 'vault') v.push(e.id)
      this.kids.set(parent, this.sortIds(v))
      this.kidsDirty.delete(parent)
    }
    return this.kids.get(parent) ?? []
  }

  private rebuildAllKids() {
    const m = new Map<string | null, string[]>()
    for (const e of this.entries.values()) {
      if (e.trashed || e.purged || e.kind === 'vault') continue
      const v = m.get(e.parent)
      if (v) v.push(e.id)
      else m.set(e.parent, [e.id])
    }
    this.kids = new Map([...m].map(([k, v]) => [k, this.sortIds(v)]))
    this.kidsDirty.clear()
  }

  private sortIds(v: string[]): string[] {
    return v.sort((a, b) => {
      const ea = this.entries.get(a)!
      const eb = this.entries.get(b)!
      const fa = ea.kind === 'folder' ? 0 : 1
      const fb = eb.kind === 'folder' ? 0 : 1
      return fa - fb || collator.compare(ea.name, eb.name) || (ea.name < eb.name ? -1 : ea.name > eb.name ? 1 : 0)
    })
  }

  /** Tree children (visibility rules applied). */
  treeChildren(parent: string | null): string[] {
    return this.liveChildren(parent).filter((id) => this.inTree(this.entries.get(id)!))
  }

  trashed(): EntryMeta[] {
    return [...this.entries.values()].filter((e) => e.trashed && !e.purged && !(e.parent && this.entries.get(e.parent)?.trashed?.batch === e.trashed.batch))
  }

  /** Quick-switcher / autocomplete candidates: documents always, images and media on request. */
  switcherCandidates(includeMedia: boolean): Candidate[] {
    if (!this.switcher) {
      const c: Candidate[] = []
      for (const e of this.entries.values()) {
        if (!isLinkable(e)) continue
        const p = (this.path(e.id) ?? e.name).normalize('NFC').toLowerCase()
        c.push({ id: e.id, key: p, mask: charMask(p) })
      }
      this.switcher = c
    }
    if (includeMedia) return this.switcher
    return this.switcher.filter((c) => {
      const e = this.entries.get(c.id)!
      return e.kind === 'markdown' || e.kind === 'pdf'
    })
  }

  static wantsMedia(q: string) {
    return q.includes('.') || isImageName(q)
  }
}
