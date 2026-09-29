import { describe, it, expect } from 'vitest'
import { search, score, charMask } from '../src/lib/fuzzy'
import { EntryStore } from '../src/stores/entries'
import { bigVault } from './util'

const cand = (id: string, s: string) => ({ id, key: s.toLowerCase(), mask: charMask(s.toLowerCase()) })

describe('fuzzy', () => {
  it('requires a subsequence', () => {
    expect(score('abc', 'a-b-c')).toBeGreaterThan(0)
    expect(score('abd', 'abc')).toBe(-1)
  })
  it('prefers word starts and consecutive runs', () => {
    const hits = search([cand('1', 'daily/meeting notes.md'), cand('2', 'my notes.md'), cand('3', 'mint notes.md')], 'mn')
    expect(hits[0].id).toBe('2')
    const h2 = search([cand('a', 'xproject.md'), cand('b', 'project.md')], 'proj')
    expect(h2[0].id).toBe('b')
  })
  it('is case- and NFC-insensitive', () => {
    const decomposed = 'Café.md'.normalize('NFD')
    const hits = search([cand('1', decomposed.normalize('NFC'))], 'CAFÉ')
    expect(hits).toHaveLength(1)
  })
  it('answers in <30 ms on 10k notes + 20k attachments', () => {
    const s = new EntryStore()
    s.load(bigVault(10_000, 20_000))
    s.switcherCandidates(true) // build once, as the app does
    for (const q of ['note 12', 'topic 5', 'pasted image 1999.png', 'zzzz', 'f1/n']) {
      const t0 = performance.now()
      search(s.switcherCandidates(EntryStore.wantsMedia(q)), q, 50)
      expect(performance.now() - t0, q).toBeLessThan(30)
    }
  })
})
