<script lang="ts">
  import type { AppState } from '../stores/app.svelte'
  import { displayName } from '../lib/names'
  import { untrack } from 'svelte'

  let { app }: { app: AppState } = $props()
  let q = $state('')
  let hits: { id: string; snippet: string; page?: number }[] = $state([])
  let busy = $state(false)
  let input: HTMLInputElement | undefined = $state()

  $effect(() => {
    input?.focus()
  })

  // One query in flight, latest wins: the first keystroke is answered at once (no debounce, DESIGN
  // §18: query → results <30 ms), and fast typing doesn't queue a query per key in the worker.
  let inFlight = false
  let next: string | null = null
  async function run(query: string) {
    inFlight = true
    try {
      const r = await app.backend.search(query)
      if (next === null && q.trim() === query) {
        hits = r.filter((h) => app.entries.get(h.id) && !app.entries.get(h.id)!.trashed)
        busy = false
      }
    } finally {
      inFlight = false
      const n = next
      next = null
      if (n !== null && n === q.trim() && n) void run(n)
    }
  }

  $effect(() => {
    void app.indexVersion // re-run when edits have been indexed
    const query = q.trim()
    if (!query) {
      hits = []
      next = null
      return
    }
    busy = untrack(() => !hits.length)
    if (inFlight) next = query
    else void run(query)
  })

  function parts(s: string) {
    // Snippets mark matches with \u0002 … \u0003 (never HTML).
    const out: { t: string; hit: boolean }[] = []
    let hit = false
    let buf = ''
    for (const ch of s) {
      if (ch === String.fromCharCode(2) || ch === String.fromCharCode(3)) {
        if (buf) out.push({ t: buf, hit })
        buf = ''
        hit = ch === String.fromCharCode(2)
      } else buf += ch
    }
    if (buf) out.push({ t: buf, hit })
    return out
  }
</script>

<div class="search">
  <input bind:this={input} type="search" data-sidebar-focus="search" placeholder="Search notes and PDFs" bind:value={q} aria-label="Search notes" data-testid="search-input" />
  {#if busy && !hits.length}<p class="muted">Searching…</p>{/if}
  <ul role="list" data-testid="search-results">
    {#each hits as h (h.id + ':' + (h.page ?? 0))}
      {@const e = app.entries.get(h.id)}
      {#if e}
        <li>
          <button onclick={() => app.open(h.id, h.page ? `#page=${h.page}` : null)}>
            <strong>{displayName(e.name)}{#if h.page}<span class="page"> · page {h.page}</span>{/if}</strong>
            <span class="snip">{#each parts(h.snippet) as p}{#if p.hit}<mark>{p.t}</mark>{:else}{p.t}{/if}{/each}</span>
          </button>
        </li>
      {/if}
    {/each}
  </ul>
  {#if q.trim() && !busy && !hits.length}<p class="muted">No results.</p>{/if}
</div>

<style>
  .page {
    font-weight: normal;
    color: var(--fg-3);
    font-size: 12px;
  }
  .search {
    display: flex;
    flex-direction: column;
    height: 100%;
    padding: 8px;
    gap: 6px;
  }
  input {
    width: 100%;
  }
  ul {
    list-style: none;
    padding: 0;
    margin: 0;
    overflow-y: auto;
    flex: 1;
  }
  button {
    display: flex;
    flex-direction: column;
    gap: 2px;
    border: 0;
    background: none;
    width: 100%;
    text-align: left;
    padding: 6px 8px;
    border-radius: 6px;
    cursor: pointer;
  }
  button:hover {
    background: var(--bg-3);
  }
  .snip {
    font-size: 12px;
    color: var(--fg-2);
    overflow: hidden;
    display: -webkit-box;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    -webkit-box-orient: vertical;
  }
  mark {
    background: rgba(255, 213, 0, 0.45);
    color: inherit;
  }
  p {
    font-size: 13px;
    padding: 0 6px;
  }
</style>
