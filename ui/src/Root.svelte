<script lang="ts">
  // Boot flow: one-tab gate → token (or auth screen) → workspace.
  import { onMount } from 'svelte'
  import App from './App.svelte'
  import Auth from './components/Auth.svelte'
  import { AppState } from './stores/app.svelte'
  import { WebBackend } from './backend/web'
  import { getToken, setToken } from './lib/auth'
  import { createTabGate } from './lib/tabgate'
  import { registerCommands } from './app-commands'
  import { writeBoot, type BootRecord } from './lib/boot'
  import { isTauri } from './lib/platform'
  import { currentSpace, type Space } from './lib/spaces'

  let { boot }: { boot: BootRecord | null } = $props()
  /** The open note's state goes in the boot record up to this size (a 1 MB note is ~1.1 MB). */
  const BOOT_DOC_MAX = 4 << 20

  type Phase = 'gate' | 'blocked' | 'lost' | 'spaces' | 'auth' | 'app'
  let phase: Phase = $state('gate')
  let app: AppState | null = $state(null)
  const gate = createTabGate()

  gate.onLost(async () => {
    await app?.backend.flush().catch(() => {})
    phase = 'lost'
    location.reload()
  })

  let space: Space | null = null

  async function start() {
    // Apps: a fresh install has no space yet; it chooses one first (DESIGN §24).
    if (isTauri) {
      space = await currentSpace()
      if (!space) {
        phase = 'spaces'
        return
      }
    }
    const token = await getToken()
    if (!token) {
      phase = 'auth'
      return
    }
    await launch(token)
  }

  async function launch(token: string) {
    const backend = isTauri ? new (await import('./backend/tauri')).TauriBackend() : new WebBackend(token)
    if (isTauri) backend.setToken(token)
    if (boot) backend.entries.load(boot.entries)
    const a = new AppState(backend)
    a.space = space
    registerCommands(a)
    app = a
    phase = 'app'
    const last = location.hash.startsWith('#/note/') ? null : (boot?.lastNote ?? a.device.lastNote)
    // The last note opens from the boot record's copy while the worker loads the vault (also when
    // the URL names it, as after a reload).
    const first = location.hash.match(/^#\/note\/([^/?#]+)/)?.[1] ?? last
    if (first && boot?.doc && boot.lastNote === first && backend instanceof WebBackend) backend.preload(first, boot.doc)
    if (last && backend.entries.get(last)) a.open(last)
    try {
      await backend.start()
      backend.setOfflineMode(a.device.offlineAttachments)
    } catch (e) {
      a.toast(`Couldn't open the local database: ${(e as Error).message}`, 'error')
    }
    if (!location.hash.startsWith('#/note/') && last && !a.active && backend.entries.get(last)) a.open(last)
    wireLifecycle(a)
  }

  function wireLifecycle(a: AppState) {
    // For the Android shell: MainActivity asks `back()` first on the back gesture (DESIGN §11.5).
    // `entryCount()` is for the device smoke test (catch-up timing), `indexReady()` for the
    // benchmarks (the search index has caught up).
    let indexReady = false
    a.backend.on((e) => {
      if (e.ev === 'indexReady') indexReady = true
    })
    ;(window as unknown as { __jess?: object }).__jess = { back: () => a.back(), entryCount: () => a.entries.entries.size, indexReady: () => indexReady }
    const saveBoot = () => {
      const snapshot = [...a.entries.entries.values()]
      const doc = a.active ? (a.backend.docState?.(a.active) ?? null) : null
      void writeBoot({ lastNote: a.active, entries: snapshot, doc: doc && doc.length <= BOOT_DOC_MAX ? doc : null, at: Date.now() })
    }
    document.addEventListener('visibilitychange', () => {
      const fg = document.visibilityState === 'visible'
      a.backend.setForeground(fg)
      if (!fg) {
        void a.backend.flush()
        saveBoot()
      }
    })
    addEventListener('pagehide', () => {
      void a.backend.flush()
      saveBoot()
    })
    addEventListener('online', () => a.backend.online())
    addEventListener('offline', () => a.backend.offline())
    let bootTimer: ReturnType<typeof setTimeout> | undefined
    const scheduleBoot = () => {
      clearTimeout(bootTimer)
      bootTimer = setTimeout(saveBoot, 2000)
    }
    a.backend.entries.subscribe(scheduleBoot)
    // Opening another note too: pagehide isn't reliable (app windows closing, mobile kills, and
    // an IndexedDB write started there may not finish before a reload).
    a.onActiveChange = scheduleBoot
    scheduleBoot()
    a.backend.on((e) => {
      if (e.ev === 'quota') {
        a.device.offlineAttachments = 'on-demand'
        a.saveDevice()
        a.toast(`This device is out of storage space. Attachments are now downloaded on demand${e.evicted ? ` (${e.evicted} removed from this device; they're safe on the server)` : ''}.`, 'error')
      }
      if (e.ev === 'fatal' && /invalid token|revoked|unauthori[sz]ed/i.test(e.message)) void logout()
    })
  }

  async function logout() {
    await setToken(null)
    location.hash = ''
    location.reload()
  }

  async function useHere() {
    await gate.takeOver()
    await start()
  }

  onMount(async () => {
    // One window in the apps: no tab gate.
    if (isTauri) return void (await start())
    // A pairing link always goes to the auth screen first.
    if (/[#&]pair=/.test(location.hash)) {
      if (await gate.tryAcquire()) phase = 'auth'
      else phase = 'blocked'
      return
    }
    if (await gate.tryAcquire()) await start()
    else phase = 'blocked'
  })
</script>

{#if phase === 'app' && app}
  <App {app} {logout} />
{:else if phase === 'spaces'}
  {#await import('./components/Spaces.svelte') then { default: Spaces }}<Spaces welcome />{/await}
{:else if phase === 'auth'}
  <Auth done={(t) => void launch(t)} />
{:else if phase === 'blocked'}
  <main class="center" data-testid="blocked">
    <p>Jess is open in another tab.</p>
    <button class="btn primary" onclick={() => void useHere()}>Use here</button>
  </main>
{:else if phase === 'lost'}
  <main class="center"><p>Jess was opened in another tab.</p></main>
{/if}

<style>
  .center {
    height: 100%;
    display: grid;
    place-content: center;
    text-align: center;
    gap: 12px;
  }
</style>
