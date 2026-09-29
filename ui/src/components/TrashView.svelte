<script lang="ts">
  import Modal from './Modal.svelte'
  import type { AppState } from '../stores/app.svelte'
  let { app }: { app: AppState } = $props()
  const items = $derived.by(() => {
    void app.version
    return app.entries.trashed().sort((a, b) => (b.trashed?.at ?? 0) - (a.trashed?.at ?? 0))
  })
  let confirming: string | null = $state(null)
</script>

<Modal title="Trash" close={() => (app.overlay = null)}>
  {#if !items.length}
    <p class="muted">Trash is empty.</p>
  {:else}
    <p class="muted small">Items are deleted permanently after the server's retention period (30 days by default).</p>
    <ul>
      {#each items as e (e.id)}
        <li>
          <span class="name">{app.entries.path(e.id)}</span>
          <span class="muted small">{new Date(e.trashed!.at).toLocaleString()}</span>
          <button class="btn" onclick={() => void app.intent([{ op: 'restore', target: e.trashed!.batch }])}>Restore</button>
          {#if confirming === e.id}
            <button class="btn danger" onclick={() => { confirming = null; void app.intent([{ op: 'purge', id: e.id }]) }}>Delete forever</button>
          {:else}
            <button class="btn" onclick={() => (confirming = e.id)}>Delete…</button>
          {/if}
        </li>
      {/each}
    </ul>
  {/if}
</Modal>

<style>
  ul {
    list-style: none;
    padding: 0;
    margin: 0;
  }
  li {
    display: flex;
    gap: 8px;
    align-items: center;
    padding: 6px 0;
    border-bottom: 1px solid var(--border);
    flex-wrap: wrap;
  }
  .name {
    flex: 1;
    min-width: 160px;
  }
  .small {
    font-size: 12px;
  }
  .danger {
    color: var(--danger);
  }
</style>
