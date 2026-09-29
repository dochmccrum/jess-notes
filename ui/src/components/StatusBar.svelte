<script lang="ts">
  // Sync status at a glance (DESIGN §5.4): synced / syncing (n) / offline (n) / error, plus
  // attachment transfer progress.
  import type { AppState } from '../stores/app.svelte'
  import type { SyncStatus } from '../lib/types'

  let { app }: { app: AppState } = $props()
  let s: SyncStatus = $state({ state: 'starting' })
  $effect(() => app.backend.sync.subscribe((v) => (s = v)))

  const label = $derived.by(() => {
    switch (s.state) {
      case 'synced':
        return 'Synced'
      case 'syncing':
        return s.pending ? `Syncing ${s.pending} change${s.pending === 1 ? '' : 's'}` : 'Syncing'
      case 'offline':
        return s.pending ? `Offline · ${s.pending} pending` : 'Offline'
      case 'error':
        return `Sync error: ${s.error ?? 'unknown'}`
      default:
        return 'Starting…'
    }
  })
  const uploads = $derived(s.uploads && s.uploads.pending > 0 ? `Uploading ${s.uploads.total - s.uploads.pending + 1} of ${s.uploads.total}` : '')
  const downloads = $derived(s.downloads ? `Downloading ${s.downloads}` : '')
</script>

<footer class="status" aria-live="polite">
  <span class="dot {s.state}" aria-hidden="true"></span>
  <span data-testid="sync-status">{label}</span>
  {#if uploads}<span class="muted">· {uploads}</span>{/if}
  {#if downloads}<span class="muted">· {downloads}</span>{/if}
  {#if s.quarantined}<button class="link" onclick={() => (app.overlay = 'settings')}>· {s.quarantined} rejected change{s.quarantined === 1 ? '' : 's'}</button>{/if}
</footer>

<style>
  .status {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 2px 10px;
    font-size: 12px;
    color: var(--fg-2);
    border-top: 1px solid var(--border);
    background: var(--bg-2);
    min-height: 24px;
  }
  .dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--fg-3);
  }
  .dot.synced {
    background: var(--ok);
  }
  .dot.syncing {
    background: var(--warn);
  }
  .dot.error {
    background: var(--danger);
  }
  .link {
    border: 0;
    background: none;
    color: var(--danger);
    cursor: pointer;
    font-size: 12px;
    padding: 0;
  }
</style>
