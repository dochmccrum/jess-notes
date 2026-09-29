<script lang="ts">
  // One sidebar, three modes (DESIGN §11.5): pinned (grid column), shortcut and hover (fixed,
  // moved only with transform), and an off-canvas drawer on touch devices.
  import type { Snippet } from 'svelte'
  import type { AppState } from '../stores/app.svelte'

  let { app, children }: { app: AppState; children: Snippet } = $props()

  const touch = typeof matchMedia !== 'undefined' && (matchMedia('(pointer: coarse)').matches || matchMedia('(hover: none)').matches)
  const mode = $derived(touch ? 'drawer' : app.device.sidebarMode)
  const open = $derived(app.device.sidebarOpen)

  function setOpen(v: boolean) {
    if (app.device.sidebarOpen !== v) {
      app.device.sidebarOpen = v
      app.saveDevice()
    }
  }

  // ---- hover-reveal: 100 ms intent in the hot zone, 300 ms grace after leaving
  let intent: ReturnType<typeof setTimeout> | undefined
  let grace: ReturnType<typeof setTimeout> | undefined
  let dragging = false
  const HOVER_INTENT = 100
  const HOVER_GRACE = 300

  function zoneEnter(e: PointerEvent) {
    if (mode !== 'hover' || e.buttons !== 0 || dragging || hasSelectionDrag()) return
    clearTimeout(intent)
    intent = setTimeout(() => setOpen(true), HOVER_INTENT)
  }
  function zoneLeave() {
    clearTimeout(intent)
  }
  function panelEnter() {
    clearTimeout(grace)
  }
  function panelLeave(e: PointerEvent) {
    if (mode !== 'hover' || !open) return
    if (e.buttons !== 0) return
    clearTimeout(grace)
    grace = setTimeout(() => setOpen(false), HOVER_GRACE)
  }
  function hasSelectionDrag() {
    const s = getSelection()
    return !!s && !s.isCollapsed && dragging
  }
  $effect(() => {
    const down = () => (dragging = true)
    const up = () => (dragging = false)
    addEventListener('pointerdown', down, true)
    addEventListener('pointerup', up, true)
    return () => {
      removeEventListener('pointerdown', down, true)
      removeEventListener('pointerup', up, true)
    }
  })

  // ---- drawer: edge swipe to open, swipe back or scrim tap to close
  let sx = 0
  let sy = 0
  let st = 0
  let tracking: 'open' | 'close' | null = null
  $effect(() => {
    if (mode !== 'drawer') return
    const start = (e: PointerEvent) => {
      if (e.pointerType === 'mouse') return
      if (!open && e.clientX <= 20) tracking = 'open'
      else if (open && (e.target as HTMLElement).closest('.sidebar')) tracking = 'close'
      else return
      sx = e.clientX
      sy = e.clientY
      st = performance.now()
    }
    const end = (e: PointerEvent) => {
      if (!tracking) return
      const dx = e.clientX - sx
      const dy = Math.abs(e.clientY - sy)
      const v = Math.abs(dx) / Math.max(1, performance.now() - st)
      if (dy < 60 && (Math.abs(dx) > 60 || v > 0.5)) {
        if (tracking === 'open' && dx > 0) setOpen(true)
        if (tracking === 'close' && dx < 0) setOpen(false)
      }
      tracking = null
    }
    addEventListener('pointerdown', start)
    addEventListener('pointerup', end)
    return () => {
      removeEventListener('pointerdown', start)
      removeEventListener('pointerup', end)
    }
  })

  // ---- width drag (pinned)
  function resize(e: PointerEvent) {
    const startX = e.clientX
    const w0 = app.device.sidebarWidth
    const move = (m: PointerEvent) => {
      app.device.sidebarWidth = Math.max(180, Math.min(600, w0 + m.clientX - startX))
    }
    const up = () => {
      removeEventListener('pointermove', move)
      removeEventListener('pointerup', up)
      app.saveDevice()
    }
    addEventListener('pointermove', move)
    addEventListener('pointerup', up)
  }
</script>

{#if mode === 'hover' && !open}
  <div class="hot-zone" role="presentation" data-testid="hot-zone" onpointerenter={zoneEnter} onpointerleave={zoneLeave}></div>
{/if}
{#if mode === 'drawer' && open}
  <div class="scrim" role="presentation" onclick={() => setOpen(false)}></div>
{/if}
<aside
  class="sidebar mode-{mode}"
  class:open
  aria-label="Sidebar"
  aria-hidden={mode !== 'pinned' && !open}
  inert={mode !== 'pinned' && !open}
  style:--sb-w="{app.device.sidebarWidth}px"
  onpointerenter={panelEnter}
  onpointerleave={panelLeave}
  data-testid="sidebar"
>
  {@render children()}
  {#if mode === 'pinned'}
    <div class="resize" role="separator" aria-orientation="vertical" aria-label="Resize sidebar" onpointerdown={resize}></div>
  {/if}
</aside>

<style>
  .sidebar {
    background: var(--bg-2);
    border-right: 1px solid var(--border);
    display: flex;
    flex-direction: column;
    min-width: 0;
    height: 100%;
    position: relative;
  }
  .mode-pinned {
    width: var(--sb-w);
  }
  .mode-pinned:not(.open) {
    display: none;
  }
  .mode-shortcut,
  .mode-hover,
  .mode-drawer {
    position: fixed;
    top: 0;
    left: 0;
    bottom: 0;
    width: var(--sb-w);
    max-width: 88vw;
    z-index: 50;
    transform: translateX(-100%);
    transition: transform 150ms ease;
    box-shadow: none;
  }
  .mode-shortcut.open,
  .mode-hover.open,
  .mode-drawer.open {
    transform: translateX(0);
    box-shadow: var(--shadow);
  }
  .hot-zone {
    position: fixed;
    left: 0;
    top: 0;
    bottom: 0;
    width: 8px;
    z-index: 49;
  }
  .scrim {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.3);
    z-index: 49;
  }
  .resize {
    position: absolute;
    top: 0;
    right: -3px;
    width: 6px;
    height: 100%;
    cursor: col-resize;
    z-index: 2;
  }
  .resize:hover {
    background: var(--accent);
    opacity: 0.4;
  }
  @media (prefers-reduced-motion: reduce) {
    .sidebar {
      transition: none;
    }
  }
</style>
