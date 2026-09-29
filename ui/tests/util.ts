import type { EntryMeta } from '../src/lib/types'

let n = 0
export function id(): string {
  const h = (++n).toString(16).padStart(12, '0')
  return `00000000-0000-7000-8000-${h}`
}

export function entry(p: Partial<EntryMeta> & { name: string }): EntryMeta {
  return { id: id(), kind: 'markdown', parent: null, trashed: null, visible: true, blob: null, created: null, modified: null, purged: false, seq: 0, props: {}, ...p }
}

/** A vault of `notes` notes and `attachments` images spread over `folders` folders. */
export function bigVault(notes: number, attachments: number, folders = 200): EntryMeta[] {
  const out: EntryMeta[] = []
  const fs: EntryMeta[] = []
  for (let i = 0; i < folders; i++) {
    const f = entry({ kind: 'folder', name: `Folder ${i}`, parent: i >= 20 ? fs[i % 20].id : null })
    fs.push(f)
    out.push(f)
  }
  for (let i = 0; i < notes; i++) out.push(entry({ name: `Note ${i} about topic ${i % 97}.md`, parent: fs[i % folders].id }))
  for (let i = 0; i < attachments; i++) out.push(entry({ kind: 'media', name: `Pasted image ${i}.png`, parent: fs[i % folders].id, visible: false, blob: 'x' }))
  return out
}
