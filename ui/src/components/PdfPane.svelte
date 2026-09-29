<script lang="ts">
  // Standalone PDF view (DESIGN §10.4): PDF.js in a lazy chunk, page n/N, zoom, find, and lazy
  // outline/thumbnails. The last page and zoom are remembered per device.
  import { onMount, tick } from 'svelte'
  import type { AppState } from '../stores/app.svelte'
  import type { PdfViewer, ViewerState } from '../pdf/viewer'

  let { app, id, hash, size }: { app: AppState; id: string; hash: string; size: number } = $props()
  let host: HTMLDivElement | undefined = $state()
  let viewer: PdfViewer | null = null
  let st: ViewerState = $state({ page: 1, pages: 0, zoom: 1, fit: true })
  let error: string | null = $state(null)
  let loading = $state(true)
  let findOpen = $state(false)
  let query = $state('')
  let found: { index: number; total: number } | null = $state(null)
  let findInput: HTMLInputElement | undefined = $state()
  let side: 'none' | 'outline' | 'thumbs' = $state('none')
  let outline: { title: string; page: number | null; depth: number }[] | null = $state(null)
  let pageInput = $state('1')
  const KEY = `jess.pdf.${id}`

  function saved(): { page?: number; zoom?: number | 'fit' } {
    try {
      return JSON.parse(localStorage.getItem(KEY) ?? '{}')
    } catch {
      return {}
    }
  }
  let saveTimer: ReturnType<typeof setTimeout> | undefined
  function save(s: ViewerState) {
    clearTimeout(saveTimer)
    saveTimer = setTimeout(() => {
      try {
        localStorage.setItem(KEY, JSON.stringify({ page: s.page, zoom: s.fit ? 'fit' : s.zoom }))
      } catch {
        /* ignore */
      }
    }, 300)
  }

  onMount(() => {
    let dead = false
    const t0 = performance.now()
    // Fetch the whole file in the background (offline copy), at "open document" priority.
    app.backend.blobWant(hash, size, 0)
    const sub = app.pendingSubpath
    const m = sub ? /page=(\d+)/.exec(sub) : null
    const prev = saved()
    void import('../pdf/viewer').then(async ({ openPdf, PdfViewer }) => {
      try {
        const doc = await openPdf({ backend: app.backend, hash, size })
        if (dead) return void doc.destroy()
        viewer = new PdfViewer(host!, doc, {
          page: m ? Number(m[1]) : prev.page,
          zoom: prev.zoom ?? 'fit',
          onState: (s) => {
            st = s
            pageInput = String(s.page)
            save(s)
          },
        })
        await viewer.init()
        loading = false
        performance.measure('open-pdf', { start: t0 })
      } catch (e) {
        error = (e as Error).message
        loading = false
      }
    })
    return () => {
      dead = true
      viewer?.destroy()
      viewer = null
    }
  })

  // Another link or search hit into this same PDF: jump to its page.
  $effect(() => {
    const m = app.pendingSubpath ? /page=(\d+)/.exec(app.pendingSubpath) : null
    if (m && viewer) viewer.goto(Number(m[1]))
  })

  async function toggleFind() {
    findOpen = !findOpen
    await tick()
    if (findOpen) findInput?.select()
  }
  async function doFind(dir: 1 | -1) {
    if (viewer) found = await viewer.find(query, dir)
  }
  async function toggleSide(which: 'outline' | 'thumbs') {
    side = side === which ? 'none' : which
    if (side === 'outline' && !outline && viewer) outline = await viewer.outline()
  }
  function thumb(node: HTMLCanvasElement, n: number) {
    const io = new IntersectionObserver((es) => {
      if (es.some((e) => e.isIntersecting)) {
        io.disconnect()
        void viewer?.renderThumb(n, node)
      }
    })
    io.observe(node)
    return { destroy: () => io.disconnect() }
  }
  function key(e: KeyboardEvent) {
    if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 'f') {
      e.preventDefault()
      void toggleFind()
    } else if (e.key === 'Escape' && findOpen) findOpen = false
  }
</script>

<svelte:window onkeydown={key} />

<div class="pdf" role="presentation" data-testid="pdf-pane">
  <div class="tools" role="toolbar" aria-label="PDF">
    <button class="icon-btn" title="Outline" aria-pressed={side === 'outline'} onclick={() => void toggleSide('outline')}>☰</button>
    <button class="icon-btn" title="Pages" aria-pressed={side === 'thumbs'} onclick={() => void toggleSide('thumbs')}>▦</button>
    <span class="pageno">
      <input aria-label="Page" inputmode="numeric" bind:value={pageInput} onchange={() => viewer?.goto(Number(pageInput))} data-testid="pdf-page" />
      / {st.pages}
    </span>
    <span class="spacer"></span>
    <button class="icon-btn" title="Zoom out" onclick={() => viewer?.zoomBy(1 / 1.2)}>−</button>
    <button class="btn small" title="Fit width" onclick={() => viewer?.applyZoom('fit')}>{st.fit ? 'Fit' : `${Math.round(st.zoom * 100)}%`}</button>
    <button class="icon-btn" title="Zoom in" onclick={() => viewer?.zoomBy(1.2)}>+</button>
    <button class="icon-btn" title="Find (Ctrl/⌘+F)" aria-pressed={findOpen} onclick={() => void toggleFind()}>⌕</button>
  </div>
  {#if findOpen}
    <div class="find">
      <input bind:this={findInput} bind:value={query} placeholder="Find in PDF" onkeydown={(e) => e.key === 'Enter' && void doFind(e.shiftKey ? -1 : 1)} data-testid="pdf-find" />
      <button class="icon-btn" aria-label="Previous" onclick={() => void doFind(-1)}>↑</button>
      <button class="icon-btn" aria-label="Next" onclick={() => void doFind(1)}>↓</button>
      <span class="muted small" data-testid="pdf-found">{found ? (found.total ? `${found.index} of ${found.total} pages` : 'No matches') : ''}</span>
    </div>
  {/if}
  <div class="body">
    {#if side !== 'none'}
      <nav class="side" aria-label={side === 'outline' ? 'Outline' : 'Pages'}>
        {#if side === 'outline'}
          {#if outline === null}
            <p class="muted small">Loading…</p>
          {:else if !outline.length}
            <p class="muted small">No outline.</p>
          {:else}
            {#each outline as o}
              <button class="ol" style:padding-left="{8 + o.depth * 12}px" disabled={o.page === null} onclick={() => o.page && viewer?.goto(o.page)}>{o.title}</button>
            {/each}
          {/if}
        {:else}
          {#each Array.from({ length: st.pages }, (_, i) => i + 1) as n (n)}
            <button class="th" class:cur={n === st.page} onclick={() => viewer?.goto(n)} aria-label="Page {n}">
              <canvas use:thumb={n} width="120" height="160"></canvas>
              <span class="small">{n}</span>
            </button>
          {/each}
        {/if}
      </nav>
    {/if}
    <div class="scroll" bind:this={host} data-testid="pdf-scroll"></div>
    {#if loading}<div class="state muted">Opening…</div>{/if}
    {#if error}<div class="state error">{error}</div>{/if}
  </div>
</div>

<style>
  .pdf {
    display: flex;
    flex-direction: column;
    height: 100%;
    min-height: 0;
  }
  .tools,
  .find {
    display: flex;
    align-items: center;
    gap: 4px;
    padding: 4px 8px;
    border-bottom: 1px solid var(--border);
    font-size: 13px;
  }
  .pageno input {
    width: 3.5em;
    text-align: right;
  }
  .spacer {
    flex: 1;
  }
  .small {
    font-size: 12px;
  }
  .body {
    flex: 1;
    min-height: 0;
    display: flex;
    position: relative;
  }
  .scroll {
    flex: 1;
    overflow: auto;
    background: var(--bg-3);
  }
  .side {
    width: 200px;
    overflow: auto;
    border-right: 1px solid var(--border);
    background: var(--bg-2);
    display: flex;
    flex-direction: column;
    padding: 4px 0;
  }
  .ol {
    text-align: left;
    border: 0;
    background: none;
    padding: 4px 8px;
    font-size: 13px;
    cursor: pointer;
  }
  .ol:hover {
    background: var(--bg-3);
  }
  .th {
    border: 2px solid transparent;
    background: none;
    margin: 4px auto;
    display: flex;
    flex-direction: column;
    align-items: center;
    cursor: pointer;
  }
  .th.cur {
    border-color: var(--accent);
  }
  .th canvas {
    background: #fff;
    box-shadow: 0 1px 3px rgba(0, 0, 0, 0.2);
  }
  .state {
    position: absolute;
    inset: 40% 0 auto;
    text-align: center;
  }
  .error {
    color: var(--danger);
  }
</style>
