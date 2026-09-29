<script lang="ts">
  // Import (folder or zip → dry-run report → conflicts → run) and export (zip or folder)
  // (DESIGN §12). Planning and all path/link logic live in core; this is only the UI.
  import Modal from './Modal.svelte'
  import type { AppState } from '../stores/app.svelte'
  import type { ImportPlanView, ImportSource } from '../backend/types'

  let { app }: { app: AppState } = $props()

  type Res = 'Overwrite' | 'KeepBoth' | 'Skip'
  let hidePdfs = $state(true)
  let busy = $state(false)
  let plan = $state<ImportPlanView | null>(null)
  let source: ImportSource | null = null
  let resolutions: Record<string, Res> = $state({})
  let applyAll: Res | '' = $state('')
  let progress: { task: string; done: number; total: number } | null = $state(null)
  let result: string | null = $state(null)
  let error: string | null = $state(null)
  let portable = $state(false)

  $effect(() =>
    app.backend.on((e) => {
      if (e.ev === 'progress') progress = { task: e.task, done: e.done, total: e.total }
    }),
  )

  const conflicts = $derived(plan ? plan.items.filter((i) => typeof i.action === 'object' && i.action && 'Conflict' in (i.action as object)) : [])
  const unresolved = $derived(applyAll ? 0 : conflicts.filter((c) => !resolutions[c.path]).length)

  async function planFrom(src: ImportSource) {
    source = src
    error = null
    result = null
    busy = true
    try {
      plan = await app.backend.importer.plan(src, { hidePdfs, conflict: 'ask' })
      resolutions = {}
    } catch (e) {
      error = String((e as Error).message ?? e)
      plan = null
    } finally {
      busy = false
    }
  }

  function pickFolder(e: Event) {
    const input = e.target as HTMLInputElement
    const files = [...(input.files ?? [])]
    if (!files.length) return
    void planFrom({ kind: 'files', files, paths: files.map((f) => (f as File & { webkitRelativePath: string }).webkitRelativePath || f.name) })
    input.value = ''
  }

  function pickZip(e: Event) {
    const input = e.target as HTMLInputElement
    const f = input.files?.[0]
    if (!f) return
    void planFrom({ kind: 'zip', file: f })
    input.value = ''
  }

  async function replan() {
    if (source) await planFrom(source)
  }

  async function runImport() {
    if (!plan) return
    busy = true
    error = null
    progress = null
    try {
      const r = (await app.backend.importer.run($state.snapshot(resolutions) as Record<string, Res>, applyAll || undefined)) as { notes?: number; cancelled?: boolean }
      result = r.cancelled ? 'Import cancelled. Items already imported were kept.' : 'Import finished.'
      plan = null
      app.toast(result)
    } catch (e) {
      error = String((e as Error).message ?? e)
    } finally {
      busy = false
      progress = null
    }
  }

  async function exportVault(toFolder: boolean) {
    busy = true
    error = null
    try {
      const { items, report } = await app.backend.exporter.files(portable)
      const files = items.filter((i) => i.kind !== 'dir')
      let done = 0
      const content = async function* () {
        for (const it of files) {
          const bytes = await app.backend.exporter.content(it)
          progress = { task: 'export', done: ++done, total: files.length }
          yield { name: it.path, input: bytes, lastModified: it.modified ? new Date(it.modified) : undefined }
        }
        if (report) yield { name: 'EXPORT-REPORT.md', input: new TextEncoder().encode(report) }
      }
      const w = window as unknown as { showDirectoryPicker?: (o?: object) => Promise<FileSystemDirectoryHandle>; showSaveFilePicker?: (o?: object) => Promise<FileSystemFileHandle> }
      if (toFolder && w.showDirectoryPicker) {
        const root = await w.showDirectoryPicker({ mode: 'readwrite' })
        for await (const f of content()) {
          const parts = f.name.split('/')
          let dir = root
          for (const p of parts.slice(0, -1)) dir = await dir.getDirectoryHandle(p, { create: true })
          const fh = await dir.getFileHandle(parts[parts.length - 1], { create: true })
          const ws = await fh.createWritable()
          await ws.write(f.input as BufferSource)
          await ws.close()
        }
      } else {
        const { downloadZip } = await import('client-zip')
        const resp = downloadZip(content())
        const name = `jess-vault-${new Date().toISOString().slice(0, 10)}.zip`
        if (w.showSaveFilePicker) {
          const fh = await w.showSaveFilePicker({ suggestedName: name, types: [{ description: 'Zip archive', accept: { 'application/zip': ['.zip'] } }] })
          const ws = await fh.createWritable()
          await resp.body!.pipeTo(ws)
        } else {
          const blob = await resp.blob()
          const a = document.createElement('a')
          a.href = URL.createObjectURL(blob)
          a.download = name
          a.click()
          setTimeout(() => URL.revokeObjectURL(a.href), 10_000)
        }
      }
      app.toast('Export finished')
    } catch (e) {
      if ((e as Error).name !== 'AbortError') error = String((e as Error).message ?? e)
    } finally {
      busy = false
      progress = null
    }
  }

  const canFolderExport = typeof window !== 'undefined' && 'showDirectoryPicker' in window
</script>

<Modal title="Import / export" close={() => (app.overlay = null)} wide>
  <section>
    <h3>Import an Obsidian vault</h3>
    <p class="small muted">Nothing is changed until you confirm. Existing notes are never overwritten without asking.</p>
    <div class="row">
      <label class="btn">Choose folder… <input type="file" webkitdirectory multiple hidden onchange={pickFolder} disabled={busy} data-testid="import-folder" /></label>
      <label class="btn">Choose .zip… <input type="file" accept=".zip,application/zip" hidden onchange={pickZip} disabled={busy} data-testid="import-zip" /></label>
    </div>
    <label class="check"><input type="checkbox" bind:checked={hidePdfs} onchange={() => void replan()} /> Hide PDFs in the attachment folder from the file tree</label>
  </section>

  {#if plan}
    {@const r = plan.report}
    <section data-testid="import-report">
      <h3>Dry run</h3>
      <ul class="report">
        <li>{r.notes} notes, {r.folders} folders</li>
        <li>{r.pdfs} PDFs ({r.pdfs_hidden} hidden), {r.images} images, {r.other_media} other files</li>
        {#if r.unchanged}<li>{r.unchanged} already present and identical (skipped)</li>{/if}
        {#if r.skipped.length}<li>{r.skipped.length} skipped <details><summary>show</summary><ul>{#each r.skipped as [p, why]}<li><code>{p}</code> — {why}</li>{/each}</ul></details></li>{/if}
        {#if r.unresolved.length}<li>{r.unresolved.length} links point at nothing (kept as-is) <details><summary>show</summary><ul>{#each r.unresolved.slice(0, 200) as [p, t]}<li><code>{p}</code> → <code>{t}</code></li>{/each}</ul></details></li>{/if}
        {#each r.warnings as w}<li class="warn">{w}</li>{/each}
      </ul>
    </section>
    {#if conflicts.length}
      <section>
        <h3>{conflicts.length} conflicting {conflicts.length === 1 ? 'file' : 'files'}</h3>
        <label>
          For all
          <select bind:value={applyAll}>
            <option value="">Decide individually</option>
            <option value="Skip">Skip (keep what's here)</option>
            <option value="KeepBoth">Keep both</option>
            <option value="Overwrite">Overwrite</option>
          </select>
        </label>
        {#if !applyAll}
          <ul class="conflicts">
            {#each conflicts.slice(0, 500) as c (c.path)}
              <li>
                <code>{c.path}</code>
                <select bind:value={resolutions[c.path]}>
                  <option value={undefined}>—</option>
                  <option value="Skip">Skip</option>
                  <option value="KeepBoth">Keep both</option>
                  <option value="Overwrite">Overwrite</option>
                </select>
              </li>
            {/each}
          </ul>
        {/if}
      </section>
    {/if}
    <section class="row">
      <button class="btn primary" disabled={busy || unresolved > 0} onclick={() => void runImport()} data-testid="import-run">Import</button>
      {#if busy}<button class="btn" onclick={() => app.backend.importer.cancel()}>Cancel</button>{/if}
      {#if unresolved}<span class="small muted">{unresolved} conflicts need a decision</span>{/if}
    </section>
  {/if}

  <section>
    <h3>Export</h3>
    <label class="check"><input type="checkbox" bind:checked={portable} /> Portable names (safe on Windows)</label>
    <div class="row">
      <button class="btn" disabled={busy} onclick={() => void exportVault(false)} data-testid="export-zip">Export as .zip</button>
      {#if canFolderExport}<button class="btn" disabled={busy} onclick={() => void exportVault(true)}>Export to folder…</button>{/if}
    </div>
  </section>

  {#if progress}
    <progress max={progress.total} value={progress.done}></progress>
    <span class="small">{progress.task}: {progress.done} / {progress.total}</span>
  {:else if busy}
    <span class="small muted">Working…</span>
  {/if}
  {#if error}<p class="error">{error}</p>{/if}
  {#if result}<p>{result}</p>{/if}
</Modal>

<style>
  section {
    margin-bottom: 16px;
  }
  h3 {
    font-size: 13px;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--fg-2);
    margin: 0 0 8px;
  }
  .row {
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
    align-items: center;
  }
  label.check {
    display: flex;
    gap: 8px;
    align-items: center;
    margin-top: 8px;
    font-size: 14px;
  }
  .small {
    font-size: 12px;
  }
  .report,
  .conflicts {
    font-size: 14px;
    padding-left: 18px;
  }
  .conflicts {
    max-height: 240px;
    overflow: auto;
    list-style: none;
    padding: 0;
  }
  .conflicts li {
    display: flex;
    justify-content: space-between;
    gap: 8px;
    padding: 2px 0;
  }
  .warn,
  .error {
    color: var(--danger);
  }
  progress {
    width: 100%;
  }
</style>
