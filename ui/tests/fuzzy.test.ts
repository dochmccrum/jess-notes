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
  it('ranks the note named by the query first, then names over paths', () => {
    const c = [cand('scattered', 'projects/others 1/matrix 2/name hand soon 3576.md'), cand('exact', 'bench/maths.md'), cand('longer', 'maths homework.md'), cand('in-path', 'maths/zz.md')]
    expect(search(c, 'maths').map((h) => h.id)).toEqual(['exact', 'longer', 'in-path', 'scattered'])
    // A consecutive run at a word start beats the same letters spread over word starts.
    expect(score('maths', 'a/maths x.md')).toBeGreaterThan(score('maths', 'a/matrix h s.md'))
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
      // Best of 3: Vitest runs test files in parallel, and one sample can land on a busy core.
      let best = Infinity
      for (let i = 0; i < 3; i++) {
        const t0 = performance.now()
        search(s.switcherCandidates(EntryStore.wantsMedia(q)), q, 50)
        best = Math.min(best, performance.now() - t0)
      }
      expect(best, q).toBeLessThan(30)
    }
  })
})
