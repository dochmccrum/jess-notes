<script lang="ts">
  import Modal from './Modal.svelte'
  import type { AppState } from '../stores/app.svelte'
  let { app }: { app: AppState } = $props()
  const req = $derived(app.prompt!)
  let value = $state(app.prompt?.value ?? '')
  const error = $derived(req.validate ? req.validate(value) : null)
  let input: HTMLInputElement | undefined = $state()
  $effect(() => {
    input?.focus()
    input?.select()
  })
  function done(v: string | null) {
    const r = req
    app.prompt = null
    r.resolve(v)
  }
</script>

<Modal title={req.title} close={() => done(null)}>
  <form onsubmit={(e) => { e.preventDefault(); if (!error && value.trim()) done(value.trim()) }}>
    <input bind:this={input} type="text" bind:value placeholder={req.placeholder ?? ''} aria-invalid={!!error} aria-describedby="prompt-err" data-testid="prompt-input" />
    <p id="prompt-err" class="err" aria-live="polite">{value && error ? error : ''}</p>
    <div class="row">
      <button type="button" class="btn" onclick={() => done(null)}>Cancel</button>
      <button type="submit" class="btn primary" disabled={!!error || !value.trim()}>OK</button>
    </div>
  </form>
</Modal>

<style>
  input {
    width: 100%;
  }
  .err {
    color: var(--danger);
    min-height: 1.4em;
    font-size: 13px;
    margin: 6px 0;
  }
  .row {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
  }
</style>
