<script lang="ts">
  import Picker, { type PickItem } from './Picker.svelte'
  import type { AppState } from '../stores/app.svelte'
  import { search } from '../lib/fuzzy'
  import { EntryStore } from '../stores/entries'
  import { displayName, validateName } from '../lib/names'

  let { app }: { app: AppState } = $props()
  let query = $state('')

  const items = $derived.by((): PickItem[] => {
    void app.version
    const q = query.trim()
    let ids: string[]
    if (!q) {
      ids = [...app.entries.entries.values()].filter((e) => (e.kind === 'markdown' || (e.kind === 'pdf' && e.visible)) && !e.trashed && !e.purged).sort((a, b) => (b.modified ?? 0) - (a.modified ?? 0)).slice(0, 50).map((e) => e.id)
    } else {
      const t0 = performance.now()
      ids = search(app.entries.switcherCandidates(EntryStore.wantsMedia(q)), q, 50).map((h) => h.id)
      performance.measure?.('switcher', { start: t0 })
    }
    const out: PickItem[] = ids.map((id) => {
      const e = app.entries.get(id)!
      return { id, label: displayName(e.name), detail: app.entries.folderOf(id) }
    })
    if (q && !validateName(q) && !out.some((o) => o.label.toLowerCase() === q.toLowerCase())) out.push({ id: '__create__', label: `Create “${q}”`, hint: 'Shift+Enter' })
    return out
  })

  function pick(it: PickItem, e: KeyboardEvent | MouseEvent) {
    app.overlay = null
    if (it.id === '__create__' || (e as KeyboardEvent).shiftKey) {
      const q = query.trim()
      if (q) void app.newNote(null, `${q}.md`)
      return
    }
    app.open(it.id)
  }
</script>

<Picker placeholder="Find or create a note…" bind:query {items} {pick} close={() => (app.overlay = null)} footer="↑↓ to navigate · Enter to open · Shift+Enter to create" />
