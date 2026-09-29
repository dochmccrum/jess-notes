import { describe, it, expect } from 'vitest'
import { EntryStore } from '../src/stores/entries'
import { entry, bigVault } from './util'

describe('EntryStore', () => {
  it('sorts folders first, then natural order', () => {
    const s = new EntryStore()
    const f = entry({ kind: 'folder', name: 'zeta' })
    const a = entry({ name: 'Note 10.md' })
    const b = entry({ name: 'Note 2.md' })
    const c = entry({ name: 'apple.md' })
    s.load([a, b, c, f])
    expect(s.treeChildren(null).map((x) => s.get(x)!.name)).toEqual(['zeta', 'apple.md', 'Note 2.md', 'Note 10.md'])
  })

  it('applies tree visibility rules (§3.1)', () => {
    const s = new EntryStore()
    const att = entry({ kind: 'folder', name: 'attachments' })
    const img = entry({ kind: 'media', name: 'a.png', parent: att.id, visible: false })
    const pdfHidden = entry({ kind: 'pdf', name: 'hidden.pdf', parent: att.id, visible: false })
    const pdf = entry({ kind: 'pdf', name: 'paper.pdf', visible: true })
    const empty = entry({ kind: 'folder', name: 'empty' })
    s.load([att, img, pdfHidden, pdf, empty])
    // A folder holding only hidden attachments is hidden; an empty folder is shown.
    expect(s.treeChildren(null).map((x) => s.get(x)!.name)).toEqual(['empty', 'paper.pdf'])
    s.showAllAttachments = true
    s.apply([])
    expect(s.treeChildren(null).map((x) => s.get(x)!.name)).toEqual(['attachments', 'empty', 'paper.pdf'])
    expect(s.treeChildren(att.id).map((x) => s.get(x)!.name)).toEqual(['a.png', 'hidden.pdf'])
  })

  it('tracks paths through folder renames and moves', () => {
    const s = new EntryStore()
    const f = entry({ kind: 'folder', name: 'Projects' })
    const g = entry({ kind: 'folder', name: 'Deep', parent: f.id })
    const n = entry({ name: 'Plan.md', parent: g.id })
    s.load([f, g, n])
    expect(s.path(n.id)).toBe('Projects/Deep/Plan.md')
    expect(s.resolver.resolve('Projects/Deep/Plan', 'wiki', '')?.id).toBe(n.id)
    s.apply([{ ...f, name: 'Work' }])
    expect(s.path(n.id)).toBe('Work/Deep/Plan.md')
    expect(s.resolver.resolve('Work/Deep/Plan', 'wiki', '')?.id).toBe(n.id)
    expect(s.resolver.resolve('Projects/Deep/Plan', 'wiki', '')).toBeNull()
    s.apply([{ ...g, parent: null }])
    expect(s.path(n.id)).toBe('Deep/Plan.md')
    expect(s.ancestors(n.id)).toEqual([g.id])
    expect(s.descendants(f.id)).toEqual([])
  })

  it('hides trashed entries and lists trash roots only', () => {
    const s = new EntryStore()
    const f = entry({ kind: 'folder', name: 'Old' })
    const n = entry({ name: 'x.md', parent: f.id })
    s.load([f, n])
    const t = { batch: 'b1', at: 1 }
    s.apply([
      { ...f, trashed: t },
      { ...n, trashed: t },
    ])
    expect(s.treeChildren(null)).toEqual([])
    expect(s.trashed().map((e) => e.id)).toEqual([f.id])
    expect(s.resolver.resolve('x', 'wiki', '')).toBeNull()
  })

  it('switcher candidates include media only when asked', () => {
    const s = new EntryStore()
    s.load([entry({ name: 'Note.md' }), entry({ kind: 'media', name: 'pic.png', visible: false }), entry({ kind: 'pdf', name: 'doc.pdf', visible: false })])
    expect(s.switcherCandidates(false).map((c) => c.key).sort()).toEqual(['doc.pdf', 'note.md'])
    expect(s.switcherCandidates(true)).toHaveLength(3)
    expect(EntryStore.wantsMedia('pic.pn')).toBe(true)
    expect(EntryStore.wantsMedia('pic')).toBe(false)
  })

  it('expanding a folder stays under 16 ms on 10k notes + 20k attachments', () => {
    const s = new EntryStore()
    const v = bigVault(10_000, 20_000)
    s.load(v)
    s.treeChildren(null) // first build
    const folders = v.filter((e) => e.kind === 'folder').slice(0, 50)
    let worst = 0
    for (const f of folders) {
      const t0 = performance.now()
      s.treeChildren(f.id)
      worst = Math.max(worst, performance.now() - t0)
    }
    expect(worst).toBeLessThan(16)
  })

  it('a folder rename with many descendants re-indexes quickly', () => {
    const s = new EntryStore()
    const v = bigVault(10_000, 20_000, 20)
    s.load(v)
    const f = v.find((e) => e.kind === 'folder')!
    const t0 = performance.now()
    s.apply([{ ...f, name: 'Renamed' }])
    expect(performance.now() - t0).toBeLessThan(200)
    const child = v.find((e) => e.parent === f.id && e.kind === 'markdown')!
    expect(s.path(child.id)!.startsWith('Renamed/')).toBe(true)
  })
})
