// App-level UI state (runes) and vault operations shared by components and commands.
import type { Backend } from '../backend/types'
import type { EditorView } from '@codemirror/view'
import type { ImageCtx } from '../editor/embeds'
import { loadDevice, saveDevice, type DeviceSettings } from './device'
import { newId } from '../lib/ids'
import { freeName, nfc, validateName, isMarkdownName } from '../lib/names'
import type { EntryMeta, MetaIntent } from '../lib/types'
import { isTouch } from '../lib/platform'
import type { Space } from '../lib/spaces'

export type Overlay = null | 'switcher' | 'palette' | 'settings' | 'import' | 'trash' | 'move' | 'attachments' | 'spaces'

export interface PromptReq {
  title: string
  value: string
  placeholder?: string
  validate?(v: string): string | null
  resolve(v: string | null): void
}

export interface Toast {
  id: number
  text: string
  kind: 'info' | 'error'
}

let toastSeq = 0

export class AppState {
  device: DeviceSettings = $state(loadDevice())
  /** The open space (apps, DESIGN §24); null on the web. */
  space: Space | null = $state(null)
  active: string | null = $state(null)
  overlay: Overlay = $state(null)
  overlayArg: string | null = $state(null)
  prompt: PromptReq | null = $state(null)
  toasts: Toast[] = $state([])
  /** Bumps whenever entries change (components derive from it). */
  version = $state(0)
  /** Bumps when the link/tag/search index has caught up with edits. */
  indexVersion = $state(0)
  sidebarTab: 'files' | 'tags' | 'search' = $state('files')
  treeFocus: string | null = $state(null)
  pendingSubpath: string | null = $state(null)
  /** The open note's editor (not reactive: set and read imperatively). */
  editorView: EditorView | null = null
  /** The image shown in the full-screen viewer. */
  viewerImage: ImageCtx | null = $state(null)

  constructor(readonly backend: Backend) {
    backend.entries.subscribe(() => {
      this.version++
    })
    backend.on((e) => {
      if (e.ev === 'indexed') this.indexVersion++
    })
    backend.entries.showAllAttachments = this.device.showAllAttachments
  }

  saveDevice() {
    saveDevice($state.snapshot(this.device) as DeviceSettings)
  }

  toast(text: string, kind: 'info' | 'error' = 'info') {
    const id = ++toastSeq
    this.toasts = [...this.toasts, { id, text, kind }]
    setTimeout(() => (this.toasts = this.toasts.filter((t) => t.id !== id)), kind === 'error' ? 7000 : 3500)
  }

  ask(req: Omit<PromptReq, 'resolve'>): Promise<string | null> {
    return new Promise((resolve) => (this.prompt = { ...req, resolve }))
  }

  get entries() {
    return this.backend.entries
  }

  async intent(ops: MetaIntent[]): Promise<boolean> {
    const r = await this.backend.intent(ops)
    if (!r.ok) this.toast(explain(r.error ?? 'failed'), 'error')
    return r.ok
  }

  /** Set by Root: keeps the cold-start record's last note current. */
  onActiveChange: (() => void) | null = null
  /** Notes opened before the current one, for the Android back gesture (most recent last). */
  private visited: string[] = []

  open(id: string | null, subpath: string | null = null, remember = true) {
    if (remember && this.active && id !== this.active) {
      this.visited.push(this.active)
      if (this.visited.length > 50) this.visited.shift()
    }
    this.active = id
    this.pendingSubpath = subpath
    this.device.lastNote = id
    // On a phone the drawer covers the note: opening one (tree, new note, a link) closes it.
    if (isTouch && id) this.device.sidebarOpen = false
    this.saveDevice()
    this.onActiveChange?.()
    if (id) {
      for (const a of this.entries.ancestors(id)) if (!this.device.expanded.includes(a)) this.device.expanded.push(a)
      const h = `#/note/${id}`
      if (location.hash !== h) history.replaceState(null, '', h)
    }
  }

  /** The Android back gesture (DESIGN §11.5): closes the innermost open thing, else goes back to
   *  the previous note. False when there was nothing to do (the app then goes to the background). */
  back(): boolean {
    const menu = document.querySelector<HTMLElement>('[role=menu]')
    if (menu) {
      menu.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }))
      return true
    }
    if (this.prompt) {
      const p = this.prompt
      this.prompt = null
      p.resolve(null)
      return true
    }
    if (this.viewerImage) {
      this.viewerImage = null
      return true
    }
    if (this.overlay) {
      this.overlay = null
      return true
    }
    if (isTouch && this.device.sidebarOpen) {
      this.device.sidebarOpen = false
      this.saveDevice()
      return true
    }
    while (this.visited.length) {
      const prev = this.visited.pop()!
      if (prev === this.active || !this.entries.get(prev)) continue
      this.open(prev, null, false)
      return true
    }
    return false
  }

  siblingsTaken(parent: string | null): Set<string> {
    return new Set(this.entries.liveChildren(parent).map((c) => nfc(this.entries.get(c)!.name)))
  }

  /** The tree's selection while the tree has keyboard focus, else the open note. */
  focusTarget(): string | null {
    const inTree = typeof document !== 'undefined' && !!document.activeElement?.closest('[role="tree"]')
    return (inTree ? this.treeFocus : null) ?? this.active
  }

  /** The folder new notes go into, based on the active note. */
  contextFolder(target?: string | null): string | null {
    const id = target ?? this.focusTarget()
    if (!id) return null
    const e = this.entries.get(id)
    if (!e) return null
    return e.kind === 'folder' ? e.id : e.parent
  }

  async newNote(parent: string | null = this.contextFolder(), name = 'Untitled.md', open = true): Promise<string | null> {
    const id = newId()
    const final = freeName(name, this.siblingsTaken(parent))
    if (!(await this.intent([{ op: 'create', id, kind: 'markdown', parent, name: final, created: Date.now() }]))) return null
    if (open) this.open(id)
    return id
  }

  async newFolder(parent: string | null = this.contextFolder()) {
    const name = await this.ask({ title: 'New folder', value: '', placeholder: 'Folder name', validate: validateName })
    if (!name) return
    const id = newId()
    if (await this.intent([{ op: 'create', id, kind: 'folder', parent, name: freeName(name, this.siblingsTaken(parent), true) }])) {
      if (parent && !this.device.expanded.includes(parent)) this.device.expanded.push(parent)
    }
  }

  async rename(id: string) {
    const e = this.entries.get(id)
    if (!e) return
    const md = e.kind === 'markdown' && isMarkdownName(e.name)
    const current = md ? e.name.slice(0, -3) : e.name
    const v = await this.ask({ title: `Rename ${e.kind === 'folder' ? 'folder' : 'note'}`, value: current, validate: validateName })
    if (!v || v === current) return
    const name = md ? `${v}.md` : v
    await this.intent([{ op: 'setName', id, name }])
  }

  async move(id: string, parent: string | null) {
    if (id === parent || (parent && this.entries.ancestors(parent).includes(id))) {
      this.toast('A folder cannot be moved into itself', 'error')
      return
    }
    await this.intent([{ op: 'setParent', id, parent }])
  }

  async trash(id: string) {
    const e = this.entries.get(id)
    if (!e) return
    if (await this.intent([{ op: 'trash', id }])) {
      this.toast(`Moved “${e.name}” to trash`)
      if (this.active === id || (this.active && this.entries.ancestors(this.active).includes(id))) this.open(null)
    }
  }

  async setVisible(id: string, visible: boolean) {
    await this.intent([{ op: 'setVisible', id, visible }])
  }

  /** Follows a link from the active note; an unresolved link creates the note (Obsidian). */
  async openLink(target: string, markdown: boolean, subpath: string | null) {
    if (/^[a-zA-Z][a-zA-Z0-9+.-]+:/.test(target)) {
      window.open(target, '_blank', 'noopener,noreferrer')
      return
    }
    if (!target) {
      this.pendingSubpath = subpath
      return
    }
    const folder = this.active ? this.entries.folderOf(this.active) : ''
    const r = this.entries.resolver.resolve(target, markdown ? 'markdown' : 'wiki', folder)
    if (r) {
      this.open(r.id, subpath)
      return
    }
    // Create it: `[[folder/Name]]` goes into that folder if it exists, else the vault root.
    const clean = target.replace(/^\/+/, '')
    const slash = clean.lastIndexOf('/')
    let parent: string | null = null
    if (slash > 0) {
      const fid = [...this.entries.entries.values()].find((e) => e.kind === 'folder' && !e.trashed && this.entries.path(e.id) === clean.slice(0, slash))
      parent = fid?.id ?? null
    }
    const base = slash >= 0 ? clean.slice(slash + 1) : clean
    const bad = validateName(base.replace(/\.md$/i, ''))
    if (bad) {
      this.toast(`Can't create “${base}”: ${bad}`, 'error')
      return
    }
    await this.newNote(parent, /\.md$/i.test(base) ? base : `${base}.md`)
  }

  entryName(e: EntryMeta) {
    return e.kind === 'markdown' ? e.name.replace(/\.md$/i, '') : e.name
  }
}

function explain(err: string): string {
  const map: Record<string, string> = {
    Cycle: 'That would put a folder inside itself',
    ParentTrashed: 'That folder is in the trash',
    ParentMissing: 'That folder no longer exists',
    BadName: 'That name is not allowed',
    Purged: 'That item was permanently deleted',
    UnknownEntry: 'That item no longer exists',
  }
  return map[err] ?? err
}
