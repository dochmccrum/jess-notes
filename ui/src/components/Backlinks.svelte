<script lang="ts">
  import type { AppState } from '../stores/app.svelte'
  import type { Backlink } from '../backend/types'
  import { displayName } from '../lib/names'

  let { app, id }: { app: AppState; id: string } = $props()
  let links: Backlink[] = $state([])
  let loading = $state(true)

  $effect(() => {
    void app.version
    const target = id
    loading = true
    let live = true
    const t = setTimeout(() => {
      app.backend.backlinks(target).then((v) => {
        if (live) {
          links = v.filter((l) => app.entries.get(l.src) && !app.entries.get(l.src)!.trashed)
          loading = false
        }
      })
    }, 150)
    return () => {
      live = false
      clearTimeout(t)
    }
  })
</script>

<section class="panel" aria-label="Backlinks">
  <h2>Backlinks <span class="muted">{links.length || ''}</span></h2>
  {#if loading && !links.length}
    <p class="muted">Looking…</p>
  {:else if !links.length}
    <p class="muted">No notes link here yet.</p>
  {:else}
    <ul>
      {#each links as l (l.src)}
        {@const e = app.entries.get(l.src)}
        {#if e}
          <li>
            <button onclick={() => app.open(l.src)}>
              <span>{displayName(e.name)}</span>
              <span class="muted">{app.entries.folderOf(l.src)}{l.embed ? ' · embeds' : ''}{l.count > 1 ? ` · ${l.count}×` : ''}</span>
            </button>
          </li>
        {/if}
      {/each}
    </ul>
  {/if}
</section>

<style>
  .panel {
    padding: 8px 12px;
    overflow-y: auto;
    height: 100%;
  }
  h2 {
    font-size: 13px;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--fg-2);
    margin: 8px 0;
  }
  ul {
    list-style: none;
    padding: 0;
    margin: 0;
  }
  button {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    width: 100%;
    border: 0;
    background: none;
    text-align: left;
    padding: 6px 8px;
    border-radius: 6px;
    cursor: pointer;
  }
  button:hover {
    background: var(--bg-3);
  }
  .muted {
    font-size: 12px;
  }
</style>
