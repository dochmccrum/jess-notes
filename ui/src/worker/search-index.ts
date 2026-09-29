// Derived index (DESIGN §4.2): links, tags and full-text search in sqlite-wasm (FTS5), persisted
// in OPFS through the SAH-pool VFS when available (else in memory, rebuilt at start). It can be
// deleted and rebuilt at any time; nothing reads it back into the vault.

/* eslint-disable @typescript-eslint/no-explicit-any */
import type { Extracted } from '../lib/types'

type DB = any

export interface IndexedLink {
  src: string
  target: string
  syntax: 'wiki' | 'markdown'
  embed: boolean
}

const SCHEMA = `
CREATE TABLE IF NOT EXISTS notes(id TEXT PRIMARY KEY, hash TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS links(src TEXT NOT NULL, ord INTEGER NOT NULL, target TEXT NOT NULL, tkey TEXT NOT NULL, markdown INTEGER NOT NULL, embed INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS links_src ON links(src);
CREATE INDEX IF NOT EXISTS links_key ON links(tkey);
CREATE TABLE IF NOT EXISTS tags(src TEXT NOT NULL, name TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS tags_src ON tags(src);
CREATE INDEX IF NOT EXISTS tags_name ON tags(name);
CREATE VIRTUAL TABLE IF NOT EXISTS fts USING fts5(id UNINDEXED, name, body, tokenize = 'unicode61 remove_diacritics 2');
`

export function targetKey(target: string): string {
  const t = target.trim()
  const base = t.slice(t.lastIndexOf('/') + 1).trim().normalize('NFC').toLowerCase()
  return base.endsWith('.md') ? base.slice(0, -3) : base
}

async function hashText(s: string): Promise<string> {
  const d = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(s))
  return Array.from(new Uint8Array(d), (b) => b.toString(16).padStart(2, '0')).join('')
}

export class SearchIndex {
  persistent = false
  private constructor(private db: DB) {}

  static async open(): Promise<SearchIndex> {
    const { default: init } = await import('@sqlite.org/sqlite-wasm')
    const sqlite3: any = await (init as any)({ print: () => {}, printErr: () => {} })
    let db: DB
    let persistent = false
    try {
      const pool = await sqlite3.installOpfsSAHPoolVfs({ name: 'jess-index' })
      db = new pool.OpfsSAHPoolDb('/index.db')
      persistent = true
    } catch {
      db = new sqlite3.oo1.DB(':memory:')
    }
    db.exec(SCHEMA)
    const ix = new SearchIndex(db)
    ix.persistent = persistent
    return ix
  }

  private rows(sql: string, bind: unknown[] = []): any[] {
    return this.db.exec({ sql, bind, rowMode: 'object', returnValue: 'resultRows' })
  }

  indexedHashes(): Map<string, string> {
    return new Map(this.rows('SELECT id, hash FROM notes').map((r) => [r.id, r.hash]))
  }

  /** Re-indexes one note. Returns false if its text hasn't changed. */
  async upsert(id: string, name: string, text: string, ex: Extracted, force = false): Promise<boolean> {
    const h = await hashText(name + '\u0000' + text)
    const cur = this.rows('SELECT hash FROM notes WHERE id = ?', [id])[0]
    if (!force && cur?.hash === h) return false
    this.db.transaction(() => {
      this.db.exec({ sql: 'DELETE FROM links WHERE src = ?', bind: [id] })
      this.db.exec({ sql: 'DELETE FROM tags WHERE src = ?', bind: [id] })
      this.db.exec({ sql: 'DELETE FROM fts WHERE id = ?', bind: [id] })
      ex.links.forEach((l, i) => {
        if (!l.target.trim()) return
        this.db.exec({ sql: 'INSERT INTO links(src, ord, target, tkey, markdown, embed) VALUES (?,?,?,?,?,?)', bind: [id, i, l.target, targetKey(l.target), l.syntax === 'markdown' ? 1 : 0, l.embed ? 1 : 0] })
      })
      for (const t of new Set(ex.tags.map((t) => t.name.toLowerCase()))) this.db.exec({ sql: 'INSERT INTO tags(src, name) VALUES (?, ?)', bind: [id, t] })
      this.db.exec({ sql: 'INSERT INTO fts(id, name, body) VALUES (?, ?, ?)', bind: [id, name, text] })
      this.db.exec({ sql: 'INSERT OR REPLACE INTO notes(id, hash) VALUES (?, ?)', bind: [id, h] })
    })
    return true
  }

  remove(id: string) {
    this.db.transaction(() => {
      for (const t of ['links', 'tags']) this.db.exec({ sql: `DELETE FROM ${t} WHERE src = ?`, bind: [id] })
      this.db.exec({ sql: 'DELETE FROM fts WHERE id = ?', bind: [id] })
      this.db.exec({ sql: 'DELETE FROM notes WHERE id = ?', bind: [id] })
    })
  }

  /** Links whose target key matches one of `keys` (candidates for backlinks). */
  linksByKeys(keys: string[]): IndexedLink[] {
    if (!keys.length) return []
    const q = `SELECT src, target, markdown, embed FROM links WHERE tkey IN (${keys.map(() => '?').join(',')})`
    return this.rows(q, keys).map((r) => ({ src: r.src, target: r.target, syntax: r.markdown ? 'markdown' : 'wiki', embed: !!r.embed }))
  }

  tags(): { name: string; srcs: string[] }[] {
    const m = new Map<string, string[]>()
    for (const r of this.rows('SELECT name, src FROM tags ORDER BY name')) {
      const v = m.get(r.name)
      if (v) v.push(r.src)
      else m.set(r.name, [r.src])
    }
    return [...m].map(([name, srcs]) => ({ name, srcs }))
  }

  notesWithTag(tag: string): string[] {
    const t = tag.toLowerCase()
    return this.rows('SELECT DISTINCT src FROM tags WHERE name = ? OR name LIKE ?', [t, t + '/%']).map((r) => r.src)
  }

  /** Full-text search; every word is a prefix query. */
  search(q: string, limit = 50): { id: string; snippet: string }[] {
    const words = q
      .normalize('NFC')
      .split(/\s+/)
      .map((w) => w.replace(/["*^:(){}]/g, ''))
      .filter(Boolean)
    if (!words.length) return []
    const match = words.map((w) => `"${w}"*`).join(' ')
    try {
      return this.rows(`SELECT id, snippet(fts, 2, '\u0002', '\u0003', '…', 12) AS s FROM fts WHERE fts MATCH ? ORDER BY bm25(fts, 0.0, 5.0, 1.0) LIMIT ?`, [match, limit]).map((r) => ({ id: r.id, snippet: r.s }))
    } catch {
      return []
    }
  }

  clear() {
    this.db.exec('DELETE FROM notes; DELETE FROM links; DELETE FROM tags; DELETE FROM fts;')
  }
}
