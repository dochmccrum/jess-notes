<script lang="ts">
  import type { AppState } from '../stores/app.svelte'
  let { app }: { app: AppState } = $props()
</script>

<div class="toasts" role="status" aria-live="polite">
  {#each app.toasts as t (t.id)}
    <div class="toast {t.kind}">{t.text}</div>
  {/each}
</div>

<style>
  .toasts {
    position: fixed;
    bottom: 36px;
    left: 50%;
    transform: translateX(-50%);
    display: flex;
    flex-direction: column;
    gap: 8px;
    z-index: 200;
    pointer-events: none;
  }
  .toast {
    background: var(--fg);
    color: var(--bg);
    padding: 8px 14px;
    border-radius: 8px;
    box-shadow: var(--shadow);
    font-size: 14px;
    max-width: 90vw;
  }
  .toast.error {
    background: var(--danger);
    color: #fff;
  }
</style>
