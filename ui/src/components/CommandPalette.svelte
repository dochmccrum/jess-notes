<script lang="ts">
  import Picker, { type PickItem } from './Picker.svelte'
  import type { AppState } from '../stores/app.svelte'
  import { all, keyFor, run } from '../lib/commands'
  import { score } from '../lib/fuzzy'

  let { app }: { app: AppState } = $props()
  let query = $state('')
  const items = $derived.by((): PickItem[] => {
    const q = query.trim().toLowerCase()
    return all()
      .filter((c) => !c.when || c.when({}))
      .map((c) => ({ c, s: q ? score(q, c.title.toLowerCase()) : 0 }))
      .filter((x) => x.s >= 0)
      .sort((a, b) => b.s - a.s || a.c.title.localeCompare(b.c.title))
      .map(({ c }) => ({ id: c.id, label: c.title, hint: keyFor(c.id) }))
  })
</script>

<Picker placeholder="Type a command…" bind:query {items} pick={(it) => { app.overlay = null; void run(it.id) }} close={() => (app.overlay = null)} />
