// The TS display grammar and resolver against the shared core fixtures.
import { describe, expect, it } from 'vitest'
import { parser as mdParser, GFM } from '@lezer/markdown'
import { obsidian } from '../src/editor/syntax'
import { Resolver } from '../src/lib/resolver'
import links from '../../core/fixtures/links.json'
import tags from '../../core/fixtures/tags.json'
import math from '../../core/fixtures/math.json'
import resolution from '../../core/fixtures/resolution.json'

const parser = mdParser.configure([GFM, obsidian])

function collect(text: string, names: string[]) {
  const out: { name: string; from: number; to: number }[] = []
  parser.parse(text).iterate({
    enter(n) {
      if (names.includes(n.name)) out.push({ name: n.name, from: n.from, to: n.to })
    },
  })
  return out
}

// Display-only differences from core (core stays authoritative for indexing):
// a `%%` comment opened mid-line and closed in a later paragraph can't be expressed with Lezer's
// per-paragraph inline parsing; the editor shows the later paragraph unstyled.
const KNOWN_DISPLAY_DIFFERENCES = new Set(['%%[[hidden]]%% [[shown]] %%\nmulti\n\n[[still hidden]]\n%% [[shown2]]'])

describe('wikilinks and embeds', () => {
  for (const c of links as { text: string; links: { syntax: string; embed: boolean; target: string; span: string }[] }[]) {
    const wiki = c.links.filter((l) => l.syntax === 'wiki' && !c.text.startsWith('---'))
    if (c.text.startsWith('---')) continue // frontmatter links are core-only
    if (KNOWN_DISPLAY_DIFFERENCES.has(c.text)) continue
    it(JSON.stringify(c.text), () => {
      const got = collect(c.text, ['WikiLink', 'Embed']).map((n) => c.text.slice(n.from, n.to)).sort()
      expect(got).toEqual(wiki.map((l) => l.span).sort())
    })
  }
})

describe('tags', () => {
  for (const c of tags as { text: string; tags: string[] }[]) {
    if (c.text.startsWith('---')) continue
    it(JSON.stringify(c.text), () => {
      const got = collect(c.text, ['Tag']).map((n) => c.text.slice(n.from + 1, n.to))
      expect(got).toEqual(c.tags)
    })
  }
})

describe('maths', () => {
  for (const c of math as { text: string; math: { span: string; display: boolean }[] }[]) {
    it(JSON.stringify(c.text), () => {
      const got = collect(c.text, ['InlineMath', 'BlockMath']).map((n) => c.text.slice(n.from, n.to))
      expect(got).toEqual(c.math.map((m) => m.span))
    })
  }
})

describe('resolver', () => {
  const r = new Resolver()
  const paths = resolution.vault as string[]
  paths.forEach((p, i) => r.insert(String(i), p))
  for (const c of resolution.cases as { target: string; syntax: 'wiki' | 'markdown'; from: string; expect: string | null }[]) {
    it(`${c.target} from ${c.from}`, () => {
      const folder = c.from.includes('/') ? c.from.slice(0, c.from.lastIndexOf('/')) : ''
      const got = r.resolve(c.target, c.syntax, folder)
      expect(got ? paths[Number(got.id)] : null).toBe(c.expect)
    })
  }
})
