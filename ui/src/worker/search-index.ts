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

/** Bump to rebuild the index from scratch (it is derived data). */
const SCHEMA_VERSION = 3

const SCHEMA = `
CREATE TABLE IF NOT EXISTS meta(k TEXT PRIMARY KEY, v TEXT);
CREATE TABLE IF NOT EXISTS notes(id TEXT PRIMARY KEY, hash TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS pdfs(id TEXT PRIMARY KEY, blob TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS links(src TEXT NOT NULL, ord INTEGER NOT NULL, target TEXT NOT NULL, tkey TEXT NOT NULL, markdown INTEGER NOT NULL, embed INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS links_src ON links(src);
CREATE INDEX IF NOT EXISTS links_key ON links(tkey);
CREATE TABLE IF NOT EXISTS tags(src TEXT NOT NULL, name TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS tags_src ON tags(src);
CREATE INDEX IF NOT EXISTS tags_name ON tags(name);
CREATE VIRTUAL TABLE IF NOT EXISTS fts USING fts5(id UNINDEXED, page UNINDEXED, name, body, tokenize = 'unicode61 remove_diacritics 2');
CREATE TABLE IF NOT EXISTS fts_ids(rid INTEGER PRIMARY KEY, id TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS fts_ids_id ON fts_ids(id);
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
    let pool: any = null
    try {
      pool = await sqlite3.installOpfsSAHPoolVfs({ name: 'jess-index' })
      db = new pool.OpfsSAHPoolDb('/index.db')
      persistent = true
    } catch {
      db = new sqlite3.oo1.DB(':memory:')
    }
    // Derived data: no fsync (a power cut can at worst cost a rebuild; the rollback journal still
    // keeps each transaction atomic if the browser dies), and a cache big enough that a batch's
    // pages are written once at commit instead of spilling and being rewritten mid-transaction.
    const tune = (d: DB) => d.exec('PRAGMA journal_mode = TRUNCATE; PRAGMA synchronous = OFF; PRAGMA temp_store = MEMORY; PRAGMA cache_size = -32768')
    let version = 0
    try {
      tune(db)
      try {
        version = Number(db.selectValue("SELECT v FROM meta WHERE k = 'schema'") ?? 0)
      } catch {
        /* no meta table: an old index */
      }
      if (version !== SCHEMA_VERSION) {
        for (const t of ['fts', 'fts_ids', 'links', 'tags', 'notes', 'pdfs', 'meta']) db.exec(`DROP TABLE IF EXISTS ${t}`)
      }
      db.exec(SCHEMA)
    } catch (e) {
      // Unreadable (e.g. corrupt after a power cut): start again from an empty index.
      if (!pool) throw e
      console.warn('jess: rebuilding the search index', e)
      db.close()
      pool.unlink('/index.db')
      db = new pool.OpfsSAHPoolDb('/index.db')
      tune(db)
      db.exec(SCHEMA)
    }
    db.exec({ sql: "INSERT OR REPLACE INTO meta(k, v) VALUES ('schema', ?)", bind: [String(SCHEMA_VERSION)] })
    const ix = new SearchIndex(db)
    ix.persistent = persistent
    return ix
  }

  // The FTS rows of an id are found through `fts_ids` (rowid → id, indexed by id): FTS5 can't
  // index its UNINDEXED `id` column, so deleting by it scanned the whole table.
  private ftsDelete(id: string) {
    this.db.exec({ sql: 'DELETE FROM fts WHERE rowid IN (SELECT rid FROM fts_ids WHERE id = ?)', bind: [id] })
    this.db.exec({ sql: 'DELETE FROM fts_ids WHERE id = ?', bind: [id] })
  }

  private ftsInsert(id: string, page: number | null, name: string, body: string) {
    this.db.exec({ sql: 'INSERT INTO fts(id, page, name, body) VALUES (?, ?, ?, ?)', bind: [id, page, name, body] })
    this.db.exec({ sql: 'INSERT INTO fts_ids(rid, id) VALUES (last_insert_rowid(), ?)', bind: [id] })
  }

  private rows(sql: string, bind: unknown[] = []): any[] {
    return this.db.exec({ sql, bind, rowMode: 'object', returnValue: 'resultRows' })
  }

  indexedHashes(): Map<string, string> {
    return new Map(this.rows('SELECT id, hash FROM notes').map((r) => [r.id, r.hash]))
  }

  /** Re-indexes one note. Returns false if its text hasn't changed. */
  async upsert(id: string, name: string, text: string, ex: Extracted, force = false): Promise<boolean> {
    return (await this.upsertMany([{ id, name, text, ex }], force)) > 0
  }

  /** Re-indexes notes in one transaction (one journal write and sync for the lot, not one per
   *  note: indexing a 10k-note vault one transaction at a time kept the worker busy with OPFS
   *  writes for minutes). Returns how many changed. */
  async upsertMany(notes: { id: string; name: string; text: string; ex: Extracted }[], force = false): Promise<number> {
    const hashes = await Promise.all(notes.map((n) => hashText(n.name + '\u0000' + n.text)))
    const changed = notes.filter((n, i) => force || this.rows('SELECT hash FROM notes WHERE id = ?', [n.id])[0]?.hash !== hashes[i])
    if (!changed.length) return 0
    const hashOf = new Map(notes.map((n, i) => [n.id, hashes[i]]))
    this.db.transaction(() => {
      for (const { id, name, text, ex } of changed) {
        this.db.exec({ sql: 'DELETE FROM links WHERE src = ?', bind: [id] })
        this.db.exec({ sql: 'DELETE FROM tags WHERE src = ?', bind: [id] })
        this.ftsDelete(id)
        ex.links.forEach((l, i) => {
          if (!l.target.trim()) return
          this.db.exec({ sql: 'INSERT INTO links(src, ord, target, tkey, markdown, embed) VALUES (?,?,?,?,?,?)', bind: [id, i, l.target, targetKey(l.target), l.syntax === 'markdown' ? 1 : 0, l.embed ? 1 : 0] })
        })
        for (const t of new Set(ex.tags.map((t) => t.name.toLowerCase()))) this.db.exec({ sql: 'INSERT INTO tags(src, name) VALUES (?, ?)', bind: [id, t] })
        this.ftsInsert(id, null, name, text)
        this.db.exec({ sql: 'INSERT OR REPLACE INTO notes(id, hash) VALUES (?, ?)', bind: [id, hashOf.get(id)] })
      }
    })
    return changed.length
  }

  /** The blob whose text is indexed for each PDF entry. */
  indexedPdfs(): Map<string, string> {
    return new Map(this.rows('SELECT id, blob FROM pdfs').map((r) => [r.id, r.blob]))
  }

  /** Indexes a PDF's text, one row per page (DESIGN §8: server-extracted), pages `from`..`to` in
   *  one transaction, so a long PDF can be indexed in slices. The first slice drops the old rows;
   *  the last records which blob is indexed (until then the PDF counts as not indexed). */
  upsertPdf(id: string, name: string, blob: string, pages: string[], from = 0, to = pages.length) {
    this.db.transaction(() => {
      if (from === 0) this.ftsDelete(id)
      for (let i = from; i < to; i++) if (pages[i].trim()) this.ftsInsert(id, i + 1, name, pages[i])
      if (to >= pages.length) this.db.exec({ sql: 'INSERT OR REPLACE INTO pdfs(id, blob) VALUES (?, ?)', bind: [id, blob] })
    })
  }

  remove(id: string) {
    this.db.transaction(() => {
      this.db.exec({ sql: 'DELETE FROM pdfs WHERE id = ?', bind: [id] })
      for (const t of ['links', 'tags']) this.db.exec({ sql: `DELETE FROM ${t} WHERE src = ?`, bind: [id] })
      this.ftsDelete(id)
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
  search(q: string, limit = 50): { id: string; snippet: string; page?: number }[] {
    const words = q
      .normalize('NFC')
      .split(/\s+/)
      .map((w) => w.replace(/["*^:(){}]/g, ''))
      .filter(Boolean)
    if (!words.length) return []
    const match = words.map((w) => `"${w}"*`).join(' ')
    try {
      return this.rows(`SELECT id, page, snippet(fts, 3, '\u0002', '\u0003', '…', 12) AS s FROM fts WHERE fts MATCH ? ORDER BY bm25(fts, 0.0, 0.0, 5.0, 1.0) LIMIT ?`, [match, limit]).map((r) => ({ id: r.id, snippet: r.s, ...(r.page ? { page: r.page } : {}) }))
    } catch {
      return []
    }
  }

  clear() {
    this.db.exec('DELETE FROM notes; DELETE FROM pdfs; DELETE FROM links; DELETE FROM tags; DELETE FROM fts; DELETE FROM fts_ids;')
  }
}
