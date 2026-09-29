<script lang="ts">
  // Virtualised WAI-ARIA tree over a flat array of visible rows (DESIGN §11.5). Expanding a
  // folder splices its cached, naturally sorted children into the array.
  import { tick } from 'svelte'
  import VirtualList from './VirtualList.svelte'
  import ContextMenu, { type MenuItem } from './ContextMenu.svelte'
  import type { AppState } from '../stores/app.svelte'
  import { run } from '../lib/commands'
  import { displayName } from '../lib/names'

  let { app }: { app: AppState } = $props()

  interface Row {
    id: string
    depth: number
    folder: boolean
    open: boolean
  }

  const expanded = $derived(new Set(app.device.expanded))
  const rows = $derived.by(() => {
    void app.version
    const out: Row[] = []
    const walk = (parent: string | null, depth: number) => {
      for (const id of app.entries.treeChildren(parent)) {
        const e = app.entries.get(id)!
        const folder = e.kind === 'folder'
        const open = folder && expanded.has(id)
        out.push({ id, depth, folder, open })
        if (open) walk(id, depth + 1)
      }
    }
    walk(null, 0)
    return out
  })

  let focusIdx = $state(0)
  let menu: { x: number; y: number; items: MenuItem[] } | null = $state(null)
  let list: HTMLElement | undefined = $state()
  const rowH = typeof matchMedia !== 'undefined' && matchMedia('(pointer: coarse)').matches ? 44 : 28

  $effect(() => {
    // Keep the focused row in range and follow the active note.
    const i = app.active ? rows.findIndex((r) => r.id === app.active) : -1
    if (i >= 0) focusIdx = i
    else if (focusIdx >= rows.length) focusIdx = Math.max(0, rows.length - 1)
  })

  function toggle(id: string, open?: boolean) {
    const has = app.device.expanded.includes(id)
    const want = open ?? !has
    if (want && !has) app.device.expanded.push(id)
    else if (!want && has) app.device.expanded = app.device.expanded.filter((x) => x !== id)
    app.saveDevice()
  }

  function activate(r: Row) {
    if (r.folder) toggle(r.id)
    else app.open(r.id)
  }

  async function focusRow(i: number) {
    focusIdx = Math.max(0, Math.min(rows.length - 1, i))
    app.treeFocus = rows[focusIdx]?.id ?? null
    await tick()
    ;(list?.querySelector(`[data-idx="${focusIdx}"]`) as HTMLElement | null)?.focus()
  }

  function onKey(e: KeyboardEvent) {
    const r = rows[focusIdx]
    if (!r) return
    switch (e.key) {
      case 'ArrowDown':
        void focusRow(focusIdx + 1)
        break
      case 'ArrowUp':
        void focusRow(focusIdx - 1)
        break
      case 'Home':
        void focusRow(0)
        break
      case 'End':
        void focusRow(rows.length - 1)
        break
      case 'ArrowRight':
        if (r.folder && !r.open) toggle(r.id, true)
        else if (r.folder && r.open) void focusRow(focusIdx + 1)
        break
      case 'ArrowLeft':
        if (r.folder && r.open) toggle(r.id, false)
        else {
          const p = app.entries.get(r.id)?.parent
          const pi = rows.findIndex((x) => x.id === p)
          if (pi >= 0) void focusRow(pi)
        }
        break
      case 'Enter':
        activate(r)
        break
      case 'F2':
        void run('entry.rename', { target: r.id })
        break
      case 'Delete':
      case 'Backspace':
        if (e.key === 'Backspace' && !(e.metaKey || e.ctrlKey)) return
        void run('entry.trash', { target: r.id })
        break
      default:
        return
    }
    e.preventDefault()
  }

  function menuFor(id: string, x: number, y: number) {
    const e = app.entries.get(id)
    if (!e) return
    const items: MenuItem[] = [
      { label: 'New note', run: () => void run('note.new', { target: id }) },
      { label: 'New folder', run: () => void run('folder.new', { target: id }) },
      { label: 'Rename…', run: () => void run('entry.rename', { target: id }) },
      { label: 'Move to…', run: () => void run('entry.move', { target: id }) },
    ]
    if (e.kind === 'pdf') items.push(e.visible ? { label: 'Hide from tree', run: () => void run('entry.hide', { target: id }) } : { label: 'Show in file tree', run: () => void run('entry.show', { target: id }) })
    items.push({ label: 'Delete', danger: true, run: () => void run('entry.trash', { target: id }) })
    menu = { x, y, items }
  }

  let press: ReturnType<typeof setTimeout> | undefined
  function pointerDown(e: PointerEvent, id: string) {
    if (e.pointerType !== 'touch') return
    const { clientX: x, clientY: y } = e
    press = setTimeout(() => menuFor(id, x, y), 500)
  }
  const cancelPress = () => clearTimeout(press)

  function icon(r: Row) {
    const e = app.entries.get(r.id)!
    if (r.folder) return r.open ? '▾' : '▸'
    if (e.kind === 'pdf') return '▤'
    if (e.kind === 'markdown') return ''
    return '◆'
  }
</script>

<div class="tree" role="tree" aria-label="Files" tabindex="-1" bind:this={list} onkeydown={onKey}>
  {#if rows.length === 0}
    <p class="empty muted">No notes yet. Press <kbd>+</kbd> to create one, or import a vault.</p>
  {:else}
    <VirtualList items={rows} rowHeight={rowH} scrollTo={focusIdx}>
      {#snippet row(r: Row, i: number)}
        {@const e = app.entries.get(r.id)!}
        <div
          class="row"
          class:active={app.active === r.id}
          class:hidden-pdf={e.kind === 'pdf' && !e.visible}
          role="treeitem"
          aria-level={r.depth + 1}
          aria-expanded={r.folder ? r.open : undefined}
          aria-selected={app.active === r.id}
          tabindex={i === focusIdx ? 0 : -1}
          data-idx={i}
          data-id={r.id}
          style:padding-left="{8 + r.depth * 14}px"
          onclick={() => {
            focusIdx = i
            app.treeFocus = r.id
            activate(r)
          }}
          oncontextmenu={(ev) => {
            ev.preventDefault()
            menuFor(r.id, ev.clientX, ev.clientY)
          }}
          onpointerdown={(ev) => pointerDown(ev, r.id)}
          onpointerup={cancelPress}
          onpointercancel={cancelPress}
          onpointermove={cancelPress}
          onfocus={() => (app.treeFocus = r.id)}
        >
          <span class="icon" aria-hidden="true">{icon(r)}</span>
          <span class="name">{r.folder ? e.name : displayName(e.name)}</span>
        </div>
      {/snippet}
    </VirtualList>
  {/if}
</div>
{#if menu}
  <ContextMenu x={menu.x} y={menu.y} items={menu.items} close={() => (menu = null)} />
{/if}

<style>
  .tree {
    height: 100%;
    outline: none;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 6px;
    height: 100%;
    padding-right: 8px;
    cursor: pointer;
    border-radius: 6px;
    margin: 0 6px;
    white-space: nowrap;
    color: var(--fg-2);
    user-select: none;
    -webkit-user-select: none;
  }
  .row:hover {
    background: var(--bg-3);
    color: var(--fg);
  }
  .row.active {
    background: var(--sel);
    color: var(--fg);
  }
  .row:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: -2px;
  }
  .icon {
    width: 14px;
    flex: none;
    text-align: center;
    color: var(--fg-3);
    font-size: 12px;
  }
  .name {
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .hidden-pdf .name {
    opacity: 0.6;
  }
  .empty {
    padding: 12px 16px;
    font-size: 14px;
  }
</style>
