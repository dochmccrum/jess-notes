<script lang="ts">
  import type { AppState } from '../stores/app.svelte'
  import { displayName } from '../lib/names'

  let { app }: { app: AppState } = $props()
  let tags: { name: string; count: number }[] = $state([])
  let selected: string | null = $state(null)
  let notes: string[] = $state([])

  $effect(() => {
    void app.version
    const t = setTimeout(async () => {
      const v = await app.backend.tags()
      tags = v
        .map((t) => ({ name: t.name, count: t.srcs.filter((s) => app.entries.get(s) && !app.entries.get(s)!.trashed).length }))
        .filter((t) => t.count > 0)
        .sort((a, b) => b.count - a.count || a.name.localeCompare(b.name))
    }, 200)
    return () => clearTimeout(t)
  })

  async function choose(t: string) {
    selected = selected === t ? null : t
    notes = selected ? (await app.backend.notesWithTag(t)).filter((s) => app.entries.get(s) && !app.entries.get(s)!.trashed) : []
  }
</script>

<div class="tags" tabindex="-1" data-sidebar-focus="tags">
  {#if !tags.length}<p class="muted">No tags yet. Type <code>#tag</code> in a note.</p>{/if}
  <ul role="list">
    {#each tags as t (t.name)}
      <li>
        <button class:on={selected === t.name} onclick={() => void choose(t.name)}><span>#{t.name}</span><span class="n">{t.count}</span></button>
        {#if selected === t.name}
          <ul class="notes">
            {#each notes as id (id)}
              <li><button onclick={() => app.open(id)}>{displayName(app.entries.get(id)?.name ?? '')}</button></li>
            {/each}
          </ul>
        {/if}
      </li>
    {/each}
  </ul>
</div>

<style>
  .tags {
    padding: 6px;
    overflow-y: auto;
    height: 100%;
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
  }
  button {
    display: flex;
    width: 100%;
    justify-content: space-between;
    border: 0;
    background: none;
    padding: 5px 8px;
    border-radius: 6px;
    cursor: pointer;
    color: var(--fg-2);
    text-align: left;
  }
  button:hover,
  button.on {
    background: var(--bg-3);
    color: var(--fg);
  }
  .n {
    color: var(--fg-3);
    font-size: 12px;
  }
  .notes {
    padding-left: 14px;
  }
  p {
    padding: 8px;
    font-size: 14px;
  }
</style>
