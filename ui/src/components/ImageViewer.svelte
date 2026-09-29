<script lang="ts">
  // Full-screen image viewer (DESIGN §10.4; lazy chunk): the original bytes, pan/zoom with
  // pointer events and CSS transforms, wheel and pinch zoom, double-click to toggle fit/100 %.
  import type { AppState } from '../stores/app.svelte'
  import type { ImageCtx } from '../editor/embeds'
  import { blobUrl } from '../lib/blobs'
  import { orientedSize } from '../editor/embeds'

  let { app, ctx }: { app: AppState; ctx: ImageCtx } = $props()
  let src: string | null = $state(null)
  let failed = $state(false)
  let scale = $state(1)
  let tx = $state(0)
  let ty = $state(0)
  let stage: HTMLDivElement | undefined = $state()
  let closeBtn: HTMLButtonElement | undefined = $state()
  const nat = orientedSize(ctx.info)

  $effect(() => {
    if (ctx.remote) src = ctx.remote
    else if (ctx.hash) {
      app.backend.blobWant(ctx.hash, ctx.info?.size ?? 0, 0)
      void blobUrl(app.backend, ctx.hash, 'orig', ctx.info).then((u) => (u ? (src = u) : (failed = true)))
    }
    const prev = document.activeElement as HTMLElement | null
    closeBtn?.focus()
    return () => prev?.focus?.()
  })

  function close() {
    app.viewerImage = null
  }

  function zoomAt(factor: number, cx: number, cy: number) {
    const r = stage!.getBoundingClientRect()
    const px = cx - r.left - r.width / 2
    const py = cy - r.top - r.height / 2
    const next = Math.min(20, Math.max(0.1, scale * factor))
    const k = next / scale
    tx = px - (px - tx) * k
    ty = py - (py - ty) * k
    scale = next
  }

  function wheel(e: WheelEvent) {
    e.preventDefault()
    zoomAt(Math.exp(-e.deltaY / 300), e.clientX, e.clientY)
  }

  const pointers = new Map<number, { x: number; y: number }>()
  let pinch0 = 0
  function down(e: PointerEvent) {
    stage!.setPointerCapture(e.pointerId)
    pointers.set(e.pointerId, { x: e.clientX, y: e.clientY })
    if (pointers.size === 2) {
      const [a, b] = [...pointers.values()]
      pinch0 = Math.hypot(a.x - b.x, a.y - b.y)
    }
  }
  function move(e: PointerEvent) {
    const p = pointers.get(e.pointerId)
    if (!p) return
    if (pointers.size === 1) {
      tx += e.clientX - p.x
      ty += e.clientY - p.y
    }
    pointers.set(e.pointerId, { x: e.clientX, y: e.clientY })
    if (pointers.size === 2) {
      const [a, b] = [...pointers.values()]
      const d = Math.hypot(a.x - b.x, a.y - b.y)
      if (pinch0 > 0) zoomAt(d / pinch0, (a.x + b.x) / 2, (a.y + b.y) / 2)
      pinch0 = d
    }
  }
  function up(e: PointerEvent) {
    pointers.delete(e.pointerId)
    if (pointers.size < 2) pinch0 = 0
  }
  function dbl(e: MouseEvent) {
    if (scale !== 1 || tx || ty) {
      scale = 1
      tx = ty = 0
    } else zoomAt(2.5, e.clientX, e.clientY)
  }
  function key(e: KeyboardEvent) {
    if (e.key === 'Escape') close()
    else if (e.key === '+' || e.key === '=') zoomAt(1.25, innerWidth / 2, innerHeight / 2)
    else if (e.key === '-') zoomAt(0.8, innerWidth / 2, innerHeight / 2)
    else if (e.key === '0') {
      scale = 1
      tx = ty = 0
    } else return
    e.preventDefault()
    e.stopPropagation()
  }
</script>

<div class="viewer" role="dialog" aria-modal="true" aria-label={ctx.alt || ctx.source} tabindex="-1" onkeydown={key} data-testid="image-viewer">
  <div class="bar">
    <span class="name">{ctx.source}{nat ? ` · ${nat[0]}×${nat[1]}` : ''}</span>
    <span class="zoom">{Math.round(scale * 100)}%</span>
    <button bind:this={closeBtn} class="icon-btn" aria-label="Close" onclick={close}>✕</button>
  </div>
  <div
    class="stage"
    bind:this={stage}
    role="presentation"
    onwheel={wheel}
    onpointerdown={down}
    onpointermove={move}
    onpointerup={up}
    onpointercancel={up}
    ondblclick={dbl}
    onclick={(e) => e.target === stage && close()}
  >
    {#if src}
      <img {src} alt={ctx.alt || ctx.source} draggable="false" style:transform="translate({tx}px, {ty}px) scale({scale})" />
    {:else if failed}
      <p class="msg">This image isn't available offline.</p>
    {:else}
      <p class="msg">Loading…</p>
    {/if}
  </div>
</div>

<style>
  .viewer {
    position: fixed;
    inset: 0;
    z-index: 200;
    background: rgba(10, 10, 12, 0.92);
    display: flex;
    flex-direction: column;
    color: #eee;
  }
  .bar {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 8px 12px;
    font-size: 13px;
  }
  .name {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .icon-btn {
    color: #eee;
  }
  .stage {
    flex: 1;
    overflow: hidden;
    display: grid;
    place-items: center;
    touch-action: none;
    cursor: grab;
  }
  img {
    max-width: 100%;
    max-height: 100%;
    transform-origin: center;
    will-change: transform;
    user-select: none;
  }
  .msg {
    opacity: 0.7;
  }
</style>
