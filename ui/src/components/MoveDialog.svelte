<script lang="ts">
  import Picker, { type PickItem } from './Picker.svelte'
  import type { AppState } from '../stores/app.svelte'
  import { search, charMask } from '../lib/fuzzy'

  let { app, id }: { app: AppState; id: string } = $props()
  let query = $state('')
  const folders = $derived.by(() => {
    void app.version
    const banned = new Set([id, ...app.entries.descendants(id)])
    return [...app.entries.entries.values()].filter((e) => e.kind === 'folder' && !e.trashed && !e.purged && !banned.has(e.id)).map((e) => ({ id: e.id, path: app.entries.path(e.id) ?? e.name }))
  })
  const items = $derived.by((): PickItem[] => {
    const root = { id: '__root__', label: '/', detail: 'Vault root' }
    if (!query.trim()) return [root, ...folders.sort((a, b) => a.path.localeCompare(b.path)).slice(0, 100).map((f) => ({ id: f.id, label: f.path }))]
    const hits = search(folders.map((f) => ({ id: f.id, key: f.path.toLowerCase(), mask: charMask(f.path.toLowerCase()) })), query, 50)
    return hits.map((h) => ({ id: h.id, label: folders.find((f) => f.id === h.id)!.path }))
  })
</script>

<Picker placeholder="Move to folder…" bind:query {items} pick={(it) => { app.overlay = null; void app.move(id, it.id === '__root__' ? null : it.id) }} close={() => (app.overlay = null)} />
