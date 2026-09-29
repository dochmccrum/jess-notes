// Adding attachments to a note (DESIGN §7.3): paste, drop, "Insert attachment". The embed text
// goes in immediately (showing a "preparing" placeholder); the worker ingests the bytes, then the
// entry is created and the embed resolves.
import type { EditorView } from '@codemirror/view'
import type { AppState } from '../stores/app.svelte'
import { VAULT_SETTINGS_ID } from '../lib/types'
import { newId } from '../lib/ids'
import { freeName, nfc } from '../lib/names'

const pending = new Map<string, number>() // lower-cased name → count

export function isPending(name: string): boolean {
  return pending.has(nfc(name).toLowerCase())
}

function pad(n: number, w = 2) {
  return String(n).padStart(w, '0')
}

/** Obsidian's name for pasted images: `Pasted image 20240131235959.png`. */
export function pastedName(d: Date, ext: string): string {
  const s = `${d.getFullYear()}${pad(d.getMonth() + 1)}${pad(d.getDate())}${pad(d.getHours())}${pad(d.getMinutes())}${pad(d.getSeconds())}`
  return `Pasted image ${s}.${ext}`
}

const EXT_BY_MIME: Record<string, string> = {
  'image/png': 'png',
  'image/jpeg': 'jpg',
  'image/gif': 'gif',
  'image/webp': 'webp',
  'image/svg+xml': 'svg',
  'image/bmp': 'bmp',
  'image/avif': 'avif',
  'image/heic': 'heic',
  'application/pdf': 'pdf',
}

/**
 * The folder attachments of `noteId` go into, per the vault's `attachmentFolderPath`
 * (Obsidian): `/` or empty → vault root; `./` → the note's folder; `./sub` → a subfolder of the
 * note's folder; anything else → that vault folder. Returns the folder's path segments.
 */
export function attachmentFolder(setting: string | undefined, noteFolder: string): string[] {
  const s = (setting ?? '/').trim()
  const split = (p: string) => p.split('/').filter(Boolean)
  if (s === '' || s === '/') return []
  if (s === './' || s === '.') return split(noteFolder)
  if (s.startsWith('./')) return [...split(noteFolder), ...split(s.slice(2))]
  return split(s)
}

/** Makes sure the folder path exists (creating missing folders); returns its id (null = root). */
async function ensureFolder(app: AppState, segs: string[]): Promise<string | null> {
  let parent: string | null = null
  for (const seg of segs) {
    const kids = app.entries.liveChildren(parent)
    const hit: string | undefined = kids.find((id) => {
      const e = app.entries.get(id)!
      return e.kind === 'folder' && nfc(e.name).toLowerCase() === nfc(seg).toLowerCase()
    })
    if (hit) {
      parent = hit
      continue
    }
    const id = newId()
    if (!(await app.intent([{ op: 'create', id, kind: 'folder', parent, name: seg }]))) throw new Error(`could not create folder ${seg}`)
    parent = id
  }
  return parent
}

export interface AddOptions {
  /** Pasted data (no meaningful file name) gets Obsidian's "Pasted image …" name. */
  pasted?: boolean
}

/** Inserts embeds for `files` at the selection and imports them. */
export async function addAttachments(app: AppState, view: EditorView, noteId: string, files: File[], opts: AddOptions = {}) {
  if (!files.length) return
  const settings = app.entries.get(VAULT_SETTINGS_ID)?.props ?? {}
  const folderSegs = attachmentFolder(settings.attachmentFolderPath as string | undefined, app.entries.folderOf(noteId))
  const useMd = settings.useMarkdownLinks === true
  const now = new Date()
  // Decide names first (deterministic suffixes), then insert all embed text at once.
  const folderPath = folderSegs.join('/')
  const existingFolder = folderSegs.length ? findFolder(app, folderSegs) : null
  const taken = existingFolder !== undefined ? app.siblingsTaken(existingFolder) : new Set<string>()
  const plans = files.map((f, i) => {
    const ext = f.name.includes('.') ? f.name.split('.').pop()!.toLowerCase() : (EXT_BY_MIME[f.type] ?? 'bin')
    const base = opts.pasted || !f.name ? pastedName(new Date(now.getTime() + i * 1000), ext) : f.name
    const name = freeName(base, taken)
    taken.add(nfc(name))
    return { file: f, name }
  })
  const links = plans.map((p) => {
    const unique = !app.entries.resolver.resolve(p.name, 'wiki', '')
    const target = unique || !folderPath ? p.name : `${folderPath}/${p.name}`
    return useMd ? `![](${encodeURI(target).replace(/\(/g, '%28').replace(/\)/g, '%29')})` : `![[${target}]]`
  })
  for (const p of plans) pending.set(nfc(p.name).toLowerCase(), (pending.get(nfc(p.name).toLowerCase()) ?? 0) + 1)
  const sel = view.state.selection.main
  const before = sel.from > 0 ? view.state.sliceDoc(sel.from - 1, sel.from) : '\n'
  const text = (before === '\n' ? '' : '\n') + links.join('\n') + '\n'
  view.dispatch({ changes: { from: sel.from, to: sel.to, insert: text }, selection: { anchor: sel.from + text.length }, scrollIntoView: true, userEvent: 'input.paste' })
  try {
    const parent = await ensureFolder(app, folderSegs)
    for (const p of plans) {
      try {
        const { hash, info } = await app.backend.ingest(p.file, p.name)
        const kind = info.mime === 'application/pdf' ? 'pdf' : 'media'
        await app.intent([{ op: 'create', id: newId(), kind, parent, name: p.name, visible: kind === 'pdf', blob: hash, blobInfo: info, created: Date.now(), modified: p.file.lastModified || Date.now() }])
      } catch (e) {
        app.toast(`Couldn't add ${p.name}: ${(e as Error).message}`, 'error')
      }
    }
  } finally {
    for (const p of plans) {
      const k = nfc(p.name).toLowerCase()
      const n = (pending.get(k) ?? 1) - 1
      if (n <= 0) pending.delete(k)
      else pending.set(k, n)
    }
  }
}

/** The folder at `segs` (case-insensitive), `null` for root, `undefined` if it doesn't exist. */
function findFolder(app: AppState, segs: string[]): string | null | undefined {
  let parent: string | null = null
  for (const seg of segs) {
    const hit: string | undefined = app.entries.liveChildren(parent).find((id) => {
      const e = app.entries.get(id)!
      return e.kind === 'folder' && nfc(e.name).toLowerCase() === nfc(seg).toLowerCase()
    })
    if (!hit) return undefined
    parent = hit
  }
  return parent
}
