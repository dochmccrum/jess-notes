import { describe, it, expect, beforeEach } from 'vitest'
import { EditorState } from '@codemirror/state'
import { EditorView } from '@codemirror/view'
import { pastedName, attachmentFolder, addAttachments, isPending } from '../src/editor/attachments'
import { parseSize, parsePdfSubpath, imageBox, orientedSize, embedFor } from '../src/editor/embeds'
import { AppState } from '../src/stores/app.svelte'
import { MemoryBackend } from '../src/backend/memory'
import { VAULT_SETTINGS_ID } from '../src/lib/types'
import { entry } from './util'

describe('attachment naming and folders (Obsidian rules)', () => {
  it('names pasted images by timestamp', () => {
    expect(pastedName(new Date(2024, 0, 31, 23, 59, 58), 'png')).toBe('Pasted image 20240131235958.png')
  })
  it('resolves attachmentFolderPath', () => {
    expect(attachmentFolder(undefined, 'a/b')).toEqual([])
    expect(attachmentFolder('/', 'a/b')).toEqual([])
    expect(attachmentFolder('./', 'a/b')).toEqual(['a', 'b'])
    expect(attachmentFolder('./assets', 'a/b')).toEqual(['a', 'b', 'assets'])
    expect(attachmentFolder('./assets', '')).toEqual(['assets'])
    expect(attachmentFolder('Attachments/img', 'a')).toEqual(['Attachments', 'img'])
  })
})

describe('embeds', () => {
  it('parses size specs and pdf subpaths', () => {
    expect(parseSize('300')).toEqual([300, null])
    expect(parseSize('300x200')).toEqual([300, 200])
    expect(parseSize('alias')).toBeNull()
    expect(parsePdfSubpath('#page=3&height=600')).toEqual({ page: 3, height: 600 })
    expect(parsePdfSubpath(null)).toEqual({ page: 1, height: null })
    expect(parsePdfSubpath('#page=0')).toEqual({ page: 1, height: null })
  })
  it('sizes images from facts, spec and orientation, capped to the column', () => {
    const info = { size: 1, width: 4000, height: 3000, orientation: 6 }
    expect(orientedSize(info)).toEqual([3000, 4000])
    expect(imageBox({ info, spec: null }, 760)).toEqual({ width: 760, height: 1013 })
    expect(imageBox({ info, spec: [300, null] })).toEqual({ width: 300, height: 400 })
    expect(imageBox({ info: null, spec: [300, 200] })).toEqual({ width: 300, height: 200 })
    expect(imageBox({ info: null, spec: [300, null] })).toBeNull()
  })
  it('picks renderers', () => {
    const img = entry({ kind: 'media', name: 'a.png', blob: 'h', blobInfo: { size: 1, mime: 'image/png', width: 10, height: 5 } })
    expect(embedFor('a.png', img, null, '300')).toMatchObject({ id: 'image', ctx: { spec: [300, null], state: 'ok', hash: 'h' } })
    expect(embedFor('a.png', img, null, 'alt text|120x40')).toMatchObject({ ctx: { alt: 'alt text', spec: [120, 40] } })
    const pdf = entry({ kind: 'pdf', name: 'p.pdf', blob: 'x' })
    expect(embedFor('p.pdf', pdf, '#page=2&height=400', null)).toMatchObject({ id: 'pdf-embed', ctx: { page: 2, height: 400 } })
    expect(embedFor('Note', entry({ name: 'Note.md' }), '#H', null)).toMatchObject({ id: 'transclusion' })
    expect(embedFor('missing.png', null, null, null)).toMatchObject({ id: 'image', ctx: { state: 'unresolved' } })
    expect(embedFor('https://x.org/a.png', null, null, null)).toMatchObject({ id: 'image', ctx: { remote: 'https://x.org/a.png' } })
  })
})

describe('addAttachments', () => {
  let app: AppState
  let backend: MemoryBackend
  beforeEach(() => {
    localStorage.clear()
    backend = new MemoryBackend([entry({ id: VAULT_SETTINGS_ID, kind: 'vault', name: '', props: { attachmentFolderPath: './assets' } })])
    app = new AppState(backend)
  })

  it('inserts the embed at once, creates the folder and entry, avoids name collisions', async () => {
    const folder = entry({ kind: 'folder', name: 'Notes' })
    const note = entry({ name: 'Trip.md', parent: folder.id })
    backend.entries.apply([folder, note])
    const view = new EditorView({ state: EditorState.create({ doc: 'Hello' }) })
    view.dispatch({ selection: { anchor: 5 } })
    const png = new File([new Uint8Array([137, 80, 78, 71])], 'image.png', { type: 'image/png' })
    const p = addAttachments(app, view, note.id, [png, png], { pasted: true })
    // Embed text is in the note before the import finishes.
    await Promise.resolve()
    await new Promise((r) => setTimeout(r, 0))
    const text = view.state.doc.toString()
    const names = [...text.matchAll(/!\[\[(Pasted image \d{14}\.png)\]\]/g)].map((m) => m[1])
    expect(names).toHaveLength(2)
    expect(names[0]).not.toBe(names[1])
    await p
    expect(isPending(names[0])).toBe(false)
    const assets = [...backend.entries.entries.values()].find((e) => e.kind === 'folder' && e.name === 'assets')!
    expect(assets.parent).toBe(folder.id)
    expect(app.toasts.map((t) => t.text)).toEqual([])
    const media = [...backend.entries.entries.values()].filter((e) => e.kind === 'media')
    expect(media.map((m) => m.name).sort()).toEqual([...names].sort())
    expect(media.every((m) => m.parent === assets.id && !m.visible && m.blob)).toBe(true)
    view.destroy()
  })
})
