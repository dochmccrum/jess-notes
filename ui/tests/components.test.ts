import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest'
import { render, fireEvent, cleanup } from '@testing-library/svelte'
import { flushSync, tick } from 'svelte'
import Harness from './Harness.svelte'
import QuickSwitcher from '../src/components/QuickSwitcher.svelte'
import { AppState } from '../src/stores/app.svelte'
import { MemoryBackend } from '../src/backend/memory'
import { registerCommands } from '../src/app-commands'
import { entry } from './util'

// Tree row text without its icon glyph.
const label = (el: HTMLElement) => (el.textContent ?? '').replace(/^[^\p{L}\p{N}]+/u, '').trim()

function makeApp(list = [entry({ name: 'One.md' })]) {
  localStorage.clear()
  const app = new AppState(new MemoryBackend(list))
  registerCommands(app)
  return app
}

// jsdom lacks these.
beforeEach(() => {
  window.matchMedia ??= ((q: string) => ({ matches: false, media: q, addEventListener() {}, removeEventListener() {} })) as unknown as typeof window.matchMedia
  HTMLDialogElement.prototype.showModal ??= function (this: HTMLDialogElement) {
    this.open = true
  }
  HTMLDialogElement.prototype.close ??= function (this: HTMLDialogElement) {
    this.open = false
  }
  Element.prototype.scrollIntoView ??= () => {}
})
afterEach(() => {
  cleanup()
  vi.useRealTimers()
})

describe('Sidebar', () => {
  it('pinned mode is a layout column that hides when toggled', async () => {
    const app = makeApp()
    const { getByTestId } = render(Harness, { app })
    const sb = getByTestId('sidebar')
    expect(sb.classList.contains('mode-pinned')).toBe(true)
    expect(sb.classList.contains('open')).toBe(true)
    app.device.sidebarOpen = false
    flushSync()
    expect(sb.classList.contains('open')).toBe(false)
  })

  it('hover mode: 100 ms intent, 300 ms grace, not while a button is held', async () => {
    vi.useFakeTimers()
    const app = makeApp()
    app.device.sidebarMode = 'hover'
    app.device.sidebarOpen = false
    const { getByTestId } = render(Harness, { app })
    const sb = getByTestId('sidebar')
    const zone = getByTestId('hot-zone')
    // Brief touch of the zone: nothing.
    await fireEvent.pointerEnter(zone, { buttons: 0 })
    vi.advanceTimersByTime(60)
    await fireEvent.pointerLeave(zone)
    vi.advanceTimersByTime(200)
    flushSync()
    expect(sb.classList.contains('open')).toBe(false)
    // Holding a mouse button (dragging a selection): nothing.
    await fireEvent.pointerEnter(zone, { buttons: 1 })
    vi.advanceTimersByTime(200)
    flushSync()
    expect(sb.classList.contains('open')).toBe(false)
    // Dwell: opens at 100 ms.
    await fireEvent.pointerEnter(zone, { buttons: 0 })
    vi.advanceTimersByTime(99)
    flushSync()
    expect(sb.classList.contains('open')).toBe(false)
    vi.advanceTimersByTime(1)
    flushSync()
    expect(sb.classList.contains('open')).toBe(true)
    // Leave: stays for 300 ms, and re-entering cancels.
    await fireEvent.pointerLeave(sb, { buttons: 0 })
    vi.advanceTimersByTime(200)
    await fireEvent.pointerEnter(sb)
    vi.advanceTimersByTime(500)
    flushSync()
    expect(sb.classList.contains('open')).toBe(true)
    await fireEvent.pointerLeave(sb, { buttons: 0 })
    vi.advanceTimersByTime(299)
    flushSync()
    expect(sb.classList.contains('open')).toBe(true)
    vi.advanceTimersByTime(1)
    flushSync()
    expect(sb.classList.contains('open')).toBe(false)
  })

  it('overlay modes are hidden from assistive tech while closed', () => {
    const app = makeApp()
    app.device.sidebarMode = 'shortcut'
    app.device.sidebarOpen = false
    const { getByTestId } = render(Harness, { app })
    const sb = getByTestId('sidebar')
    expect(sb.getAttribute('aria-hidden')).toBe('true')
    expect(sb.hasAttribute('inert') || (sb as unknown as { inert?: boolean }).inert === true).toBe(true)
  })
})

describe('FileTree', () => {
  it('keyboard: arrows move, Right expands, Left collapses, Enter opens', async () => {
    const f = entry({ kind: 'folder', name: 'Folder' })
    const inner = entry({ name: 'Inner.md', parent: f.id })
    const top = entry({ name: 'Top.md' })
    const app = makeApp([f, inner, top])
    const { getByTestId, findAllByRole } = render(Harness, { app, tree: true })
    const tree = getByTestId('tree')
    let items = await findAllByRole('treeitem')
    expect(items.map((i) => label(i))).toEqual(['Folder', 'Top'])
    items[0].focus()
    await fireEvent.keyDown(items[0], { key: 'ArrowRight' })
    await tick()
    items = await findAllByRole('treeitem')
    expect(items.map((i) => label(i))).toEqual(['Folder', 'Inner', 'Top'])
    expect(items[0].getAttribute('aria-expanded')).toBe('true')
    await fireEvent.keyDown(tree, { key: 'ArrowDown' })
    await tick()
    await fireEvent.keyDown(tree, { key: 'Enter' })
    expect(app.active).toBe(inner.id)
    await fireEvent.keyDown(tree, { key: 'ArrowLeft' })
    await tick()
    await fireEvent.keyDown(tree, { key: 'ArrowLeft' })
    await tick()
    items = await findAllByRole('treeitem')
    expect(items.map((i) => label(i))).toEqual(['Folder', 'Top'])
  })
})

describe('FileTree visibility', () => {
  it('hides media, shows visible PDFs, and hides a PDF from the context menu', async () => {
    const pdf = entry({ kind: 'pdf', name: 'Paper.pdf', visible: true })
    const img = entry({ kind: 'media', name: 'pic.png', visible: false })
    const note = entry({ name: 'Note.md' })
    const app = makeApp([pdf, img, note])
    const { findAllByRole, getByRole, queryByText } = render(Harness, { app, tree: true })
    let items = await findAllByRole('treeitem')
    expect(items.map((i) => label(i))).toEqual(['Note', 'Paper.pdf'])
    expect(queryByText('pic.png')).toBeNull()
    await fireEvent.contextMenu(items[1])
    await fireEvent.click(getByRole('menuitem', { name: 'Hide from tree' }))
    await tick()
    items = await findAllByRole('treeitem')
    expect(items.map((i) => label(i))).toEqual(['Note'])
    // "Show all attachments" brings hidden files back, where they can be shown again.
    app.device.showAllAttachments = true
    app.backend.entries.showAllAttachments = true
    app.backend.entries.apply([])
    await tick()
    items = await findAllByRole('treeitem')
    expect(items.map((i) => label(i))).toEqual(['Note', 'Paper.pdf', 'pic.png'])
    await fireEvent.contextMenu(items[1])
    await fireEvent.click(getByRole('menuitem', { name: 'Show in file tree' }))
    expect(app.backend.entries.get(pdf.id)!.visible).toBe(true)
  })
})

describe('QuickSwitcher', () => {
  it('finds notes fuzzily and offers to create', async () => {
    const a = entry({ name: 'Meeting notes.md' })
    const b = entry({ name: 'Groceries.md' })
    const app = makeApp([a, b])
    app.overlay = 'switcher'
    const { getByRole, findAllByRole } = render(QuickSwitcher, { app })
    const input = getByRole('combobox')
    await fireEvent.input(input, { target: { value: 'mtng' } })
    let opts = await findAllByRole('option')
    expect(opts[0].textContent).toContain('Meeting notes')
    await fireEvent.input(input, { target: { value: 'Brand new' } })
    opts = await findAllByRole('option')
    expect(opts[opts.length - 1].textContent).toContain('Create “Brand new”')
    await fireEvent.keyDown(input, { key: 'Enter', shiftKey: true })
    await tick()
    await new Promise((r) => setTimeout(r, 0))
    const created = [...app.entries.entries.values()].find((e) => e.name === 'Brand new.md')
    expect(created).toBeTruthy()
    expect(app.active).toBe(created!.id)
  })
})
