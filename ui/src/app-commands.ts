// The app's command set and default keybindings (DESIGN §11.6). Everything the user can do from
// the keyboard, the palette or a context menu goes through here.
import { register, bind, applyOverrides, type CommandContext } from './lib/commands'
import type { AppState } from './stores/app.svelte'

export function registerCommands(app: AppState) {
  const target = (ctx: CommandContext) => ctx.target ?? app.focusTarget()
  const entry = (ctx: CommandContext) => {
    const id = target(ctx)
    return id ? app.entries.get(id) : undefined
  }
  const toggleOverlay = (o: NonNullable<typeof app.overlay>) => {
    app.overlay = app.overlay === o ? null : o
  }

  register({ id: 'note.new', title: 'New note', run: (ctx) => void app.newNote(ctx.target ? app.contextFolder(ctx.target) : app.contextFolder()) })
  register({ id: 'folder.new', title: 'New folder', run: (ctx) => void app.newFolder(ctx.target ? app.contextFolder(ctx.target) : app.contextFolder()) })
  register({ id: 'entry.rename', title: 'Rename…', when: (ctx) => !!entry(ctx), run: (ctx) => void app.rename(target(ctx)!) })
  register({
    id: 'entry.move',
    title: 'Move to folder…',
    when: (ctx) => !!entry(ctx),
    run: (ctx) => {
      app.overlayArg = target(ctx)!
      app.overlay = 'move'
    },
  })
  register({ id: 'entry.trash', title: 'Move to trash', when: (ctx) => !!entry(ctx), run: (ctx) => void app.trash(target(ctx)!) })
  register({ id: 'entry.hide', title: 'Hide from file tree', hidden: true, when: (ctx) => !!entry(ctx)?.visible, run: (ctx) => void app.setVisible(target(ctx)!, false) })
  register({ id: 'entry.show', title: 'Show in file tree', hidden: true, when: (ctx) => entry(ctx)?.visible === false, run: (ctx) => void app.setVisible(target(ctx)!, true) })
  register({
    id: 'sidebar.toggle',
    title: 'Toggle sidebar',
    run: () => {
      app.device.sidebarOpen = !app.device.sidebarOpen
      app.saveDevice()
    },
  })
  register({ id: 'switcher', title: 'Quick switcher', run: () => toggleOverlay('switcher') })
  register({ id: 'palette', title: 'Command palette', hidden: true, run: () => toggleOverlay('palette') })
  register({ id: 'settings', title: 'Open settings', run: () => toggleOverlay('settings') })
  register({ id: 'trash.open', title: 'Open trash', run: () => toggleOverlay('trash') })
  register({ id: 'attachments.open', title: 'Manage attachments', run: () => toggleOverlay('attachments') })
  register({ id: 'import.open', title: 'Import / export vault…', run: () => toggleOverlay('import') })
  register({
    id: 'panel.backlinks',
    title: 'Toggle backlinks panel',
    run: () => {
      app.device.rightPanel = !app.device.rightPanel
      app.saveDevice()
    },
  })
  const sideTab = (t: typeof app.sidebarTab) => () => {
    app.sidebarTab = t
    if (!app.device.sidebarOpen) {
      app.device.sidebarOpen = true
      app.saveDevice()
    }
    queueMicrotask(() => (document.querySelector(`[data-sidebar-focus="${t}"]`) as HTMLElement | null)?.focus())
  }
  register({ id: 'search', title: 'Search in all notes', run: sideTab('search') })
  register({ id: 'tags', title: 'Show tags', run: sideTab('tags') })
  register({ id: 'files', title: 'Show files', run: sideTab('files') })
  register({ id: 'reveal', title: 'Reveal active note in file tree', when: () => !!app.active, run: () => {
    sideTab('files')()
    app.treeFocus = app.active
  } })
  register({ id: 'sync.flush', title: 'Sync now', run: async () => {
    app.backend.online()
    await app.backend.flush()
  } })

  /** A file input: in the Android WebView it opens the system picker (the photo picker for
   *  images/videos on Android 13+); the picked files arrive as ordinary File objects. */
  const pickAndAttach = (accept?: string) => () => {
    const view = app.editorView
    const note = app.active
    if (!view || !note) return
    const input = document.createElement('input')
    input.type = 'file'
    input.multiple = true
    if (accept) input.accept = accept
    input.onchange = () => {
      const files = [...(input.files ?? [])]
      void import('./editor/attachments').then((m) => m.addAttachments(app, view, note, files))
    }
    input.click()
  }
  const editing = () => !!app.editorView && !!app.active
  register({ id: 'attachment.insert', title: 'Insert attachment…', when: editing, run: pickAndAttach() })
  register({ id: 'attachment.insertPhoto', title: 'Insert photo or video…', when: editing, run: pickAndAttach('image/*,video/*') })

  bind('Mod-n', 'note.new')
  bind('Mod-\\', 'sidebar.toggle')
  bind('Mod-o', 'switcher')
  bind('Mod-p', 'palette')
  bind('Mod-,', 'settings')
  bind('Shift-Mod-f', 'search')
  bind('Shift-Mod-b', 'panel.backlinks')
  bind('F2', 'entry.rename')
  applyOverrides(app.device.keybindings)
}
