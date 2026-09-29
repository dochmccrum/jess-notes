<script lang="ts">
  // The workspace (DESIGN §11.1): sidebar | pane | optional right panel, status bar, overlays.
  import Sidebar from './components/Sidebar.svelte'
  import FileTree from './components/FileTree.svelte'
  import TagsPanel from './components/TagsPanel.svelte'
  import SearchPanel from './components/SearchPanel.svelte'
  import EditorPane from './components/EditorPane.svelte'
  import Backlinks from './components/Backlinks.svelte'
  import StatusBar from './components/StatusBar.svelte'
  import QuickSwitcher from './components/QuickSwitcher.svelte'
  import MoveDialog from './components/MoveDialog.svelte'
  import Prompt from './components/Prompt.svelte'
  import Toasts from './components/Toasts.svelte'
  import type { AppState } from './stores/app.svelte'
  import { handleKey, run } from './lib/commands'
  import { setRenderEnv } from './editor/renderers'


  let { app, logout }: { app: AppState; logout(): void } = $props()

  // Renderers (images, PDFs) reach the backend and the app through this.
  // svelte-ignore state_referenced_locally
  setRenderEnv({
    backend: app.backend,
    openImage: (ctx) => (app.viewerImage = ctx),
    openEntry: (id, sub) => app.open(id, sub ?? null),
  })

  const activeLive = $derived.by(() => {
    void app.version
    return app.active && app.entries.get(app.active) ? app.active : null
  })

  // Capture phase: app-level bindings win over the editor's own (as in Obsidian).
  function onKey(e: KeyboardEvent) {
    if (e.defaultPrevented || e.isComposing) return
    if (app.prompt || app.viewerImage) return
    handleKey(e, {})
  }

  function route() {
    const m = /^#\/note\/([0-9a-f-]{36})/.exec(location.hash)
    if (m && m[1] !== app.active) app.open(m[1])
  }

  $effect(() => {
    route()
    addEventListener('hashchange', route)
    return () => removeEventListener('hashchange', route)
  })

  $effect(() =>
    app.backend.on((e) => {
      if (e.ev === 'rejected') app.toast(`A change was rejected by the server (${e.reason}). It's kept under Settings → Rejected changes.`, 'error')
      if (e.ev === 'fatal') app.toast(e.message, 'error')
    }),
  )

  // Rarely used panels are separate chunks (DESIGN §11.8); fetch them once the app is idle so
  // opening one is instant and keystrokes typed right after the shortcut aren't lost.
  $effect(() => {
    const idle = (cb: () => void) => ('requestIdleCallback' in window ? requestIdleCallback(cb, { timeout: 3000 }) : setTimeout(cb, 1500))
    idle(() => {
      void import('./components/CommandPalette.svelte')
      void import('./components/Settings.svelte')
      void import('./components/TrashView.svelte')
      void import('./components/ImportExport.svelte')
    })
  })

  const tabs = [
    ['files', 'Files'],
    ['search', 'Search'],
    ['tags', 'Tags'],
  ] as const
</script>

<svelte:window onkeydowncapture={onKey} />

<div class="shell" class:pinned={app.device.sidebarMode === 'pinned'}>
  <Sidebar {app}>
    <div class="sb-head">
      <div class="tabs" role="tablist">
        {#each tabs as [t, label]}
          <button role="tab" class="tab" aria-selected={app.sidebarTab === t} onclick={() => (app.sidebarTab = t)} data-testid="tab-{t}">{label}</button>
        {/each}
      </div>
      <div class="actions">
        <button class="icon-btn" title="New note" aria-label="New note" onclick={() => void run('note.new')} data-testid="new-note">＋</button>
        <button class="icon-btn" title="New folder" aria-label="New folder" onclick={() => void run('folder.new')} data-testid="new-folder">⊞</button>
        <button class="icon-btn" title="Settings" aria-label="Settings" onclick={() => void run('settings')} data-testid="open-settings">⚙</button>
      </div>
    </div>
    <div class="sb-body">
      {#if app.sidebarTab === 'files'}
        <FileTree {app} />
      {:else if app.sidebarTab === 'search'}
        <SearchPanel {app} />
      {:else}
        <TagsPanel {app} />
      {/if}
    </div>
  </Sidebar>

  <main class="main">
    {#if activeLive}
      {#key activeLive}
        <EditorPane {app} id={activeLive} />
      {/key}
    {:else}
      <div class="welcome">
        <button class="icon-btn menu" aria-label="Toggle sidebar" onclick={() => void run('sidebar.toggle')}>☰</button>
        <div class="welcome-body">
          <p class="muted">No note open.</p>
          <p>
            <button class="btn" onclick={() => void run('switcher')}>Open a note</button>
            <button class="btn" onclick={() => void run('note.new')}>New note</button>
          </p>
        </div>
      </div>
    {/if}
  </main>

  {#if app.device.rightPanel && activeLive}
    <aside class="right">
      <Backlinks {app} id={activeLive} />
    </aside>
  {/if}

  <StatusBar {app} />
</div>

{#if app.overlay === 'switcher'}
  <QuickSwitcher {app} />
{:else if app.overlay === 'palette'}
  {#await import('./components/CommandPalette.svelte') then { default: CommandPalette }}<CommandPalette {app} />{/await}
{:else if app.overlay === 'settings'}
  {#await import('./components/Settings.svelte') then { default: Settings }}<Settings {app} {logout} />{/await}
{:else if app.overlay === 'import'}
  {#await import('./components/ImportExport.svelte') then { default: ImportExport }}<ImportExport {app} />{/await}
{:else if app.overlay === 'trash'}
  {#await import('./components/TrashView.svelte') then { default: TrashView }}<TrashView {app} />{/await}
{:else if app.overlay === 'move' && app.overlayArg}
  <MoveDialog {app} id={app.overlayArg} />
{/if}
{#if app.prompt}
  {#key app.prompt}
    <Prompt {app} />
  {/key}
{/if}
{#if app.viewerImage}
  {#await import('./components/ImageViewer.svelte') then { default: ImageViewer }}<ImageViewer {app} ctx={app.viewerImage} />{/await}
{/if}
<Toasts {app} />

<style>
  .shell {
    display: grid;
    grid-template-columns: auto minmax(0, 1fr) auto;
    grid-template-rows: minmax(0, 1fr) auto;
    height: 100%;
  }
  .shell > :global(.status) {
    grid-column: 1 / -1;
  }
  .main {
    min-width: 0;
    min-height: 0;
    grid-column: 2;
  }
  .right {
    width: 280px;
    border-left: 1px solid var(--border);
    background: var(--bg-2);
    overflow: auto;
    grid-column: 3;
  }
  @media (max-width: 900px) {
    .right {
      display: none;
    }
  }
  .sb-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 6px 8px;
    border-bottom: 1px solid var(--border);
    gap: 4px;
  }
  .tabs {
    display: flex;
    gap: 2px;
  }
  .tab {
    border: 0;
    background: none;
    padding: 4px 8px;
    border-radius: 6px;
    font-size: 13px;
    color: var(--fg-2);
    cursor: pointer;
  }
  .tab[aria-selected='true'] {
    background: var(--bg-3);
    color: var(--fg);
  }
  .actions {
    display: flex;
  }
  .sb-body {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
  }
  .welcome {
    height: 100%;
    display: flex;
    flex-direction: column;
  }
  .welcome .menu {
    align-self: flex-start;
    margin: 4px 8px;
  }
  .welcome-body {
    flex: 1;
    display: grid;
    place-content: center;
    text-align: center;
  }
</style>
