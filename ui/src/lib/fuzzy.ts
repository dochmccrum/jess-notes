// fzf-style subsequence matching with bonuses (DESIGN §11.4). A char bitmask pre-filter keeps
// 30k candidates well under 30 ms.

export interface Candidate {
  id: string
  key: string // lower-cased NFC haystack (name or path)
  mask: number
}

export function charMask(s: string): number {
  let m = 0
  for (let i = 0; i < s.length; i++) {
    const c = s.charCodeAt(i)
    if (c >= 97 && c <= 122) m |= 1 << (c - 97)
    else if (c >= 48 && c <= 57) m |= 1 << 26
    else m |= 1 << 27
  }
  return m
}

/** Score of `q` as a subsequence of `s` (both lower-case; `s` a path), or -1. The note's own name
 *  counts most: an exact name first, then a name prefix, then a match inside the name, then the
 *  rest of the path. Within a match, a consecutive run keeps the bonus of the word start it began
 *  at (as in fzf), so "maths" as a word beats m-a-t-h-s spread over several word starts. */
export function score(q: string, s: string): number {
  if (!q) return 0
  const slash = s.lastIndexOf('/') + 1
  const dot = s.endsWith('.md') ? s.length - 3 : s.length
  if (s.length - slash === q.length + (s.length - dot) && s.startsWith(q, slash)) return 1e6 - s.length
  let qi = 0
  let total = 0
  let prev = -2
  let first = -1
  let run = 0
  for (let i = 0; i < s.length && qi < q.length; i++) {
    if (s[i] !== q[qi]) continue
    const b = i === 0 ? ' ' : s[i - 1]
    const start = b === ' ' || b === '/' || b === '-' || b === '_' || b === '.' ? 8 : 0
    if (i === prev + 1) run = Math.max(run, start, 5)
    else run = start
    total += 1 + run
    if (first < 0) first = i
    prev = i
    qi++
  }
  if (qi < q.length) return -1
  let bonus = 0
  if (s.startsWith(q, slash)) bonus = 1e5
  else if (first >= slash) bonus = 40
  return bonus + total * 4 - first - s.length / 16
}

export function search(cands: Candidate[], query: string, limit = 50): { id: string; score: number }[] {
  const q = query.normalize('NFC').toLowerCase().trim()
  if (!q) return []
  const qm = charMask(q)
  const out: { id: string; score: number }[] = []
  for (const c of cands) {
    if ((c.mask & qm) !== qm) continue
    const s = score(q, c.key)
    if (s >= 0) out.push({ id: c.id, score: s })
  }
  out.sort((a, b) => b.score - a.score)
  return out.slice(0, limit)
}
