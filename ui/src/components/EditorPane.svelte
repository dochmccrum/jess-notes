<script lang="ts">
  // The workspace's single pane (DESIGN §11.1): view registry by kind. The editor is created
  // imperatively; nothing in Svelte state changes on keystrokes.
  import type { AppState } from '../stores/app.svelte'
  import type { EditorView } from '@codemirror/view'
  import { createEditor } from '../editor/setup'
  import type { DocSession } from '../backend/types'
  import { displayName } from '../lib/names'
  import { run } from '../lib/commands'
  import { embedFor } from '../editor/embeds'
  import { refreshLinks } from '../editor/livepreview'
  import { addAttachments } from '../editor/attachments'
  import PdfPane from './PdfPane.svelte'
  import { blobUrl } from '../lib/blobs'

  // Images opened directly (from the tree with "show all attachments", or a link).
  let imageSrc: string | undefined = $state()
  $effect(() => {
    const e = app.entries.get(id)
    if (e?.kind === 'media' && e.blob) void blobUrl(app.backend, e.blob, 'display', e.blobInfo).then((u) => (imageSrc = u ?? undefined))
  })

  let { app, id }: { app: AppState; id: string } = $props()
  let host: HTMLDivElement | undefined = $state()
  let error: string | null = $state(null)
  const entry = $derived.by(() => {
    void app.version
    return app.entries.get(id)
  })
  const path = $derived.by(() => {
    void app.version
    return app.entries.path(id) ?? ''
  })

  $effect(() => {
    const e = app.entries.get(id)
    if (!host || !e || e.kind !== 'markdown' || e.blob) return
    let view: EditorView | null = null
    let session: DocSession | null = null
    let cancelled = false
    error = null
    const t0 = performance.now()
    app.backend.openDoc(id).then(
      (s) => {
        if (cancelled) return s.dispose()
        session = s
        view = createEditor(host!, {
          ydoc: s.ydoc,
          store: app.entries,
          entryId: id,
          links: {
            resolve: (target, markdown) => app.entries.resolver.resolve(target, markdown ? 'markdown' : 'wiki', app.entries.folderOf(id))?.id ?? null,
            open: (target, markdown, subpath) => void app.openLink(target, markdown, subpath),
            embedRenderer: (target, resolved, subpath, display) => embedFor(target, resolved ? (app.entries.get(resolved) ?? null) : null, subpath, display),
          },
          onFiles: (v, files, pasted) => void addAttachments(app, v, id, files, { pasted }),
        })
        app.editorView = view
        performance.mark('note-visible')
        performance.measure('open-note', { start: t0 })
        if (!app.pendingSubpath) view.focus()
        else scrollToSubpath(view, app.pendingSubpath)
      },
      (e) => (error = String(e)),
    )
    return () => {
      cancelled = true
      if (app.editorView === view) app.editorView = null
      view?.destroy()
      session?.dispose()
    }
  })

  // Entries changed (a pasted image's entry appeared, a target was renamed, blob facts arrived):
  // re-resolve links and embeds, at most once per frame.
  let refreshQueued = false
  $effect(() => {
    void app.version
    if (refreshQueued) return
    refreshQueued = true
    requestAnimationFrame(() => {
      refreshQueued = false
      app.editorView?.dispatch({ effects: refreshLinks.of(null) })
    })
  })

  function scrollToSubpath(view: EditorView, sub: string) {
    const h = sub.replace(/^#/, '').trim().toLowerCase()
    const doc = view.state.doc
    for (let i = 1; i <= doc.lines; i++) {
      const l = doc.line(i)
      if (/^#{1,6}\s/.test(l.text) && l.text.replace(/^#+\s*/, '').trim().toLowerCase() === h) {
        view.dispatch({ selection: { anchor: l.from }, scrollIntoView: true })
        break
      }
      if (h.startsWith('^') && l.text.trimEnd().endsWith(h)) {
        view.dispatch({ selection: { anchor: l.from }, scrollIntoView: true })
        break
      }
    }
  }

  const crumbs = $derived(path.split('/'))
</script>

<section class="pane" aria-label="Note">
  <header class="bar">
    <button class="icon-btn menu" aria-label="Toggle sidebar" onclick={() => void run('sidebar.toggle')}>☰</button>
    <nav class="crumbs" aria-label="Path">
      {#each crumbs as c, i}
        <span class:last={i === crumbs.length - 1}>{i === crumbs.length - 1 ? displayName(c) : c}</span>
        {#if i < crumbs.length - 1}<span class="sep">/</span>{/if}
      {/each}
    </nav>
    <button class="icon-btn" aria-label="Backlinks" title="Backlinks" aria-pressed={app.device.rightPanel} onclick={() => void run('panel.backlinks')}>⇆</button>
  </header>
  {#if !entry}
    <div class="empty muted">This note no longer exists.</div>
  {:else if entry.trashed}
    <div class="banner">
      This note is in the trash.
      <button class="btn" onclick={() => void app.intent([{ op: 'restore', target: id }])}>Restore</button>
    </div>
  {/if}
  {#if entry && entry.kind === 'markdown' && !entry.blob}
    <div class="editor" bind:this={host} data-testid="editor"></div>
  {:else if entry && entry.kind === 'markdown'}
    <div class="empty muted">This note isn't valid UTF-8, so it's read-only here. It's kept and exported byte-for-byte.</div>
  {:else if entry && entry.kind === 'pdf' && entry.blob}
    {#key entry.blob}
      <PdfPane {app} {id} hash={entry.blob} size={entry.blobInfo?.size ?? 0} />
    {/key}
  {:else if entry && entry.kind === 'media' && entry.blob && (entry.blobInfo?.mime ?? '').startsWith('image/')}
    <div class="image-page">
      <img src={imageSrc} alt={entry.name} width={entry.blobInfo?.width ?? undefined} height={entry.blobInfo?.height ?? undefined} />
    </div>
  {:else if entry}
    <div class="empty muted">{entry.name}</div>
  {/if}
  {#if error}<div class="banner error">{error}</div>{/if}
</section>

<style>
  .pane {
    display: flex;
    flex-direction: column;
    height: 100%;
    min-width: 0;
  }
  .bar {
    display: flex;
    align-items: center;
    gap: 4px;
    padding: 4px 8px;
    border-bottom: 1px solid var(--border);
    min-height: 40px;
  }
  .crumbs {
    flex: 1;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    color: var(--fg-3);
    font-size: 13px;
  }
  .crumbs .last {
    color: var(--fg);
  }
  .sep {
    margin: 0 4px;
  }
  .editor {
    flex: 1;
    min-height: 0;
    overflow: hidden;
  }
  .empty {
    padding: 40px;
    text-align: center;
  }
  .banner {
    padding: 8px 16px;
    background: var(--bg-2);
    border-bottom: 1px solid var(--border);
    display: flex;
    gap: 12px;
    align-items: center;
    font-size: 14px;
  }
  .image-page {
    flex: 1;
    overflow: auto;
    display: grid;
    place-items: center;
    padding: 16px;
  }
  .image-page img {
    max-width: 100%;
    height: auto;
  }
  .banner.error {
    color: var(--danger);
  }
</style>
