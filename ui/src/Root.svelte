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

  let { boot }: { boot: BootRecord | null } = $props()

  type Phase = 'gate' | 'blocked' | 'lost' | 'auth' | 'app'
  let phase: Phase = $state('gate')
  let app: AppState | null = $state(null)
  const gate = createTabGate()

  gate.onLost(async () => {
    await app?.backend.flush().catch(() => {})
    phase = 'lost'
    location.reload()
  })

  async function start() {
    const token = await getToken()
    if (!token) {
      phase = 'auth'
      return
    }
    await launch(token)
  }

  async function launch(token: string) {
    const backend = new WebBackend(token)
    if (boot) backend.entries.load(boot.entries)
    const a = new AppState(backend)
    registerCommands(a)
    app = a
    phase = 'app'
    const last = location.hash.startsWith('#/note/') ? null : (boot?.lastNote ?? a.device.lastNote)
    if (last && backend.entries.get(last)) a.open(last)
    try {
      await backend.start()
    } catch (e) {
      a.toast(`Couldn't open the local database: ${(e as Error).message}`, 'error')
    }
    if (!location.hash.startsWith('#/note/') && last && !a.active && backend.entries.get(last)) a.open(last)
    wireLifecycle(a)
  }

  function wireLifecycle(a: AppState) {
    const saveBoot = () => {
      const snapshot = [...a.entries.entries.values()]
      void writeBoot({ lastNote: a.active, entries: snapshot, doc: null, at: Date.now() })
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
    let bootTimer: ReturnType<typeof setTimeout> | undefined
    a.backend.entries.subscribe(() => {
      clearTimeout(bootTimer)
      bootTimer = setTimeout(saveBoot, 2000)
    })
    a.backend.on((e) => {
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
