<script lang="ts">
  // Attachments manager (DESIGN §7.8): every image, PDF and other file with its size, how many
  // notes link to it (live or trashed notes count), and whether its bytes are on this device.
  // Unreferenced attachments can be moved to the trash; nothing is deleted outright here.
  import Modal from './Modal.svelte'
  import VirtualList from './VirtualList.svelte'
  import type { AppState } from '../stores/app.svelte'
  import type { EntryMeta } from '../lib/types'

  let { app }: { app: AppState } = $props()
  let refs: Record<string, number> | null = $state(null)
  let filter: 'all' | 'unreferenced' | 'images' | 'pdfs' | 'other' = $state('all')
  let q = $state('')
  let confirming = $state(false)

  // Recounted when entries change and when note text has been indexed: on a device still catching
  // up, notes whose text arrives after the panel opened would otherwise leave their attachments
  // marked unused (and offered for the trash).
  let asked = 0
  $effect(() => {
    void app.version
    void app.indexVersion
    const n = ++asked
    void app.backend.attachmentRefs().then((r) => {
      if (n === asked) refs = r
    })
  })

  const all = $derived.by(() => {
    void app.version
    return [...app.entries.entries.values()].filter((e) => (e.kind === 'media' || e.kind === 'pdf') && !e.purged && !e.trashed)
  })
  const isImage = (e: EntryMeta) => (e.blobInfo?.mime ?? '').startsWith('image/')
  const rows = $derived.by(() => {
    const query = q.trim().toLowerCase()
    return all
      .filter((e) => {
        if (query && !(app.entries.path(e.id) ?? e.name).toLowerCase().includes(query)) return false
        switch (filter) {
          case 'unreferenced':
            return refs !== null && (refs[e.id] ?? 0) === 0
          case 'images':
            return isImage(e)
          case 'pdfs':
            return e.kind === 'pdf'
          case 'other':
            return e.kind === 'media' && !isImage(e)
          default:
            return true
        }
      })
      .sort((a, b) => (b.blobInfo?.size ?? 0) - (a.blobInfo?.size ?? 0))
  })
  const unreferenced = $derived(refs ? all.filter((e) => (refs![e.id] ?? 0) === 0) : [])
  const total = $derived(rows.reduce((n, e) => n + (e.blobInfo?.size ?? 0), 0))

  function fmt(n: number) {
    if (n < 1024) return `${n} B`
    if (n < 1 << 20) return `${(n / 1024).toFixed(0)} KB`
    if (n < 1 << 30) return `${(n / (1 << 20)).toFixed(1)} MB`
    return `${(n / (1 << 30)).toFixed(2)} GB`
  }

  async function trashAll() {
    confirming = false
    const ids = unreferenced.map((e) => e.id)
    for (let i = 0; i < ids.length; i += 200) await app.intent(ids.slice(i, i + 200).map((id) => ({ op: 'trash' as const, id })))
    app.toast(`Moved ${ids.length} unreferenced attachment${ids.length === 1 ? '' : 's'} to trash`)
  }
</script>

<Modal title="Attachments" close={() => (app.overlay = null)} wide>
  <div class="bar">
    <input type="search" placeholder="Filter by name or folder" bind:value={q} aria-label="Filter attachments" />
    <select bind:value={filter} aria-label="Show" data-testid="attachments-filter">
      <option value="all">All</option>
      <option value="unreferenced">Unreferenced</option>
      <option value="images">Images</option>
      <option value="pdfs">PDFs</option>
      <option value="other">Other files</option>
    </select>
  </div>
  <p class="small muted">{rows.length} files · {fmt(total)}{refs ? ` · ${unreferenced.length} not linked from any note` : ' · counting links…'}</p>
  <div class="list" data-testid="attachments-list">
    <VirtualList items={rows} rowHeight={36}>
      {#snippet row(e: EntryMeta)}
        {@const n = refs ? (refs[e.id] ?? 0) : null}
        <div class="row" class:orphan={n === 0}>
          <button class="name link" title={app.entries.path(e.id) ?? e.name} onclick={() => { app.overlay = null; app.open(e.id) }}>{e.name}</button>
          <span class="folder muted">{app.entries.folderOf(e.id) || '/'}</span>
          <span class="size">{fmt(e.blobInfo?.size ?? 0)}</span>
          <span class="refs" title="Links from notes">{n === null ? '…' : n === 0 ? 'unused' : `${n} link${n === 1 ? '' : 's'}`}</span>
          <button class="icon-btn" title="Move to trash" aria-label="Move {e.name} to trash" onclick={() => void app.trash(e.id)}>🗑</button>
        </div>
      {/snippet}
    </VirtualList>
  </div>
  {#if unreferenced.length}
    <div class="foot">
      {#if confirming}
        <span>Move {unreferenced.length} unreferenced attachment{unreferenced.length === 1 ? '' : 's'} to the trash? You can restore them from the trash.</span>
        <button class="btn" onclick={() => (confirming = false)}>Cancel</button>
        <button class="btn danger" onclick={() => void trashAll()} data-testid="trash-unreferenced-confirm">Move to trash</button>
      {:else}
        <button class="btn" onclick={() => (confirming = true)} data-testid="trash-unreferenced">Move unreferenced to trash…</button>
      {/if}
    </div>
  {/if}
</Modal>

<style>
  .bar {
    display: flex;
    gap: 8px;
  }
  .bar input {
    flex: 1;
  }
  .small {
    font-size: 12px;
  }
  .list {
    height: min(60vh, 520px);
    display: flex;
    flex-direction: column;
    border: 1px solid var(--border);
    border-radius: 6px;
  }
  .row {
    display: grid;
    grid-template-columns: minmax(0, 2fr) minmax(0, 1.2fr) 70px 70px 32px;
    gap: 8px;
    align-items: center;
    height: 36px;
    padding: 0 8px;
    font-size: 13px;
    border-bottom: 1px solid var(--border);
  }
  .row.orphan .refs {
    color: var(--warn);
  }
  .name {
    text-align: left;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    border: 0;
    background: none;
    cursor: pointer;
    color: var(--link);
    padding: 0;
  }
  .folder {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .size,
  .refs {
    text-align: right;
  }
  .foot {
    display: flex;
    gap: 8px;
    align-items: center;
    margin-top: 10px;
    font-size: 13px;
  }
  .danger {
    color: var(--danger);
  }
</style>
