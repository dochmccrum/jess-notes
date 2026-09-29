<script lang="ts">
  import { onMount, type Snippet } from 'svelte'
  let { title, close, children, wide = false }: { title: string; close(): void; children: Snippet; wide?: boolean } = $props()
  let dlg: HTMLDialogElement | undefined = $state()
  let prev: Element | null = null
  onMount(() => {
    prev = document.activeElement
    dlg!.showModal()
    return () => (prev as HTMLElement | null)?.focus?.()
  })
</script>

<dialog bind:this={dlg} class:wide aria-label={title} oncancel={(e) => { e.preventDefault(); close() }}>
  <header>
    <h2>{title}</h2>
    <button class="icon-btn" aria-label="Close" onclick={close}>✕</button>
  </header>
  <div class="body">{@render children()}</div>
</dialog>

<style>
  dialog {
    width: min(560px, 94vw);
    max-height: 86vh;
  }
  dialog.wide {
    width: min(760px, 96vw);
  }
  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 12px 16px;
    border-bottom: 1px solid var(--border);
  }
  h2 {
    font-size: 16px;
    margin: 0;
  }
  .body {
    padding: 16px;
    overflow-y: auto;
    max-height: calc(86vh - 60px);
  }
</style>
