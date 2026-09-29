// A small synchronous link resolver for the editor (DESIGN §9.2), mirroring core::resolve and
// tested against core/fixtures/resolution.json. The worker's WASM resolver remains the authority
// for backlinks and indexes; this one styles links while typing.

import { lookupKey, nfc } from './names'

export type Syntax = 'wiki' | 'markdown'
export type Via = 'relative' | 'absolute' | 'suffix' | 'basename'

function nameKeys(pathLower: string): string[] {
  const base = pathLower.slice(pathLower.lastIndexOf('/') + 1)
  return base.endsWith('.md') ? [base, base.slice(0, -3)] : [base]
}

export function join(folder: string, rel: string): string | null {
  const parts = folder.split('/').filter(Boolean)
  for (const seg of rel.split('/')) {
    if (seg === '' || seg === '.') continue
    if (seg === '..') {
      if (!parts.length) return null
      parts.pop()
    } else parts.push(seg)
  }
  return parts.join('/')
}

export function isExternal(s: string): boolean {
  const m = /^([a-zA-Z][a-zA-Z0-9+.-]*):/.exec(s)
  return !!m && m[1].length > 1
}

export class Resolver {
  private byPath = new Map<string, string[]>()
  private byName = new Map<string, string[]>()
  private paths = new Map<string, string>()

  insert(id: string, path: string) {
    this.remove(id)
    const pl = lookupKey(path)
    for (const k of nameKeys(pl)) push(this.byName, k, id)
    push(this.byPath, pl, id)
    this.paths.set(id, path)
  }

  remove(id: string) {
    const p = this.paths.get(id)
    if (p === undefined) return
    const pl = lookupKey(p)
    for (const k of nameKeys(pl)) pull(this.byName, k, id)
    pull(this.byPath, pl, id)
    this.paths.delete(id)
  }

  clear() {
    this.byPath.clear()
    this.byName.clear()
    this.paths.clear()
  }

  path(id: string) {
    return this.paths.get(id)
  }

  private exact(key: string): string[] {
    return [...(this.byPath.get(key) ?? []), ...(this.byPath.get(key + '.md') ?? [])]
  }

  private pick(cands: string[], folderLower: string, exact: string): string | undefined {
    const uniq = [...new Set(cands)]
    let best: string | undefined
    let bestRank: (string | number | boolean)[] | undefined
    for (const id of uniq) {
      const p = this.paths.get(id) ?? ''
      const pl = lookupKey(p)
      const folder = pl.includes('/') ? pl.slice(0, pl.lastIndexOf('/')) : ''
      const pn = nfc(p)
      const ends = (s: string) => pn === s || pn.endsWith('/' + s)
      const exactCase = !!exact && (ends(exact) || ends(exact + '.md'))
      const rank = [folder !== folderLower, !exactCase, (pl.match(/\//g) ?? []).length, p.length, p, id]
      if (!bestRank || cmp(rank, bestRank) < 0) {
        best = id
        bestRank = rank
      }
    }
    return best
  }

  resolve(target: string, syntax: Syntax, sourceFolder: string): { id: string; via: Via } | null {
    const t = target.trim()
    if (!t || isExternal(t)) return null
    const key = lookupKey(t)
    const folder = lookupKey(sourceFolder)
    const tn = nfc(t)
    const exactAbs = tn.replace(/^\/+/, '')
    const exactRel = join(nfc(sourceFolder), tn) ?? ''
    const one = (c: string[], via: Via) => {
      const id = this.pick(c, folder, via === 'relative' ? exactRel : exactAbs)
      return id ? { id, via } : null
    }
    if (syntax === 'markdown') {
      const rel = join(folder, key)
      if (rel !== null) {
        const r = one(this.exact(rel), 'relative')
        if (r) return r
      }
      const r = one(this.exact(key.replace(/^\/+/, '')), 'absolute')
      if (r) return r
    }
    if (key.startsWith('./') || key.startsWith('../')) {
      const rel = join(folder, key)
      return rel === null ? null : one(this.exact(rel), 'relative')
    }
    if (key.startsWith('/')) return one(this.exact(key.slice(1)), 'absolute')
    if (key.includes('/')) {
      const r = one(this.exact(key), 'absolute')
      if (r) return r
      const rel = join(folder, key)
      if (rel !== null) {
        const r2 = one(this.exact(rel), 'relative')
        if (r2) return r2
      }
      const base = key.slice(key.lastIndexOf('/') + 1)
      const cands = (this.byName.get(base) ?? []).filter((id) => {
        const pl = lookupKey(this.paths.get(id) ?? '')
        return pl.endsWith('/' + key) || pl.endsWith('/' + key + '.md')
      })
      return one(cands, 'suffix')
    }
    return one(this.byName.get(key) ?? [], 'basename')
  }
}

function push(m: Map<string, string[]>, k: string, id: string) {
  const v = m.get(k)
  if (!v) m.set(k, [id])
  else if (!v.includes(id)) v.push(id)
}
function pull(m: Map<string, string[]>, k: string, id: string) {
  const v = m.get(k)
  if (!v) return
  const i = v.indexOf(id)
  if (i >= 0) v.splice(i, 1)
  if (!v.length) m.delete(k)
}
function cmp(a: (string | number | boolean)[], b: (string | number | boolean)[]): number {
  for (let i = 0; i < a.length; i++) {
    const x = a[i]
    const y = b[i]
    if (x === y) continue
    // Rust orders strings by UTF-8 bytes / code points; compare by code point, not locale.
    if (typeof x === 'string' && typeof y === 'string') return x < y ? -1 : 1
    return (x as number) < (y as number) ? -1 : 1
  }
  return 0
}
