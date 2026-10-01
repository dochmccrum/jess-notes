<script lang="ts">
  // First-run setup, login and pairing-link redemption (DESIGN §14). In the apps, the first
  // step is the server address (or a pairing link, which carries it).
  import { onMount } from 'svelte'
  import { authState, getServer, login, redeem, setServer, setup, type AuthState } from '../lib/auth'
  import { getSpace, setSpace, type Space } from '../lib/spaces'
  import { isTauri } from '../lib/platform'

  let { done }: { done(token: string | null, space: Space): void } = $props()
  let space = $state<Space>({ id: 'local', name: 'Local Space', server: null })
  let auth = $state<AuthState | null>(null)
  let password = $state('')
  let confirm = $state('')
  let code = $state('')
  let error: string | null = $state(null)
  let busy = $state(false)
  let offline = $state(false)
  let needServer = $state(false)
  let serverInput = $state('')

  async function finish(t: string) {
    done(t, space)
  }

  async function connect() {
    offline = false
    try {
      auth = await authState()
    } catch {
      offline = true
    }
  }

  async function useLocal() {
    const name = (prompt('Name this Space', space.name) ?? '').trim()
    if (!name) return
    space = { id: 'local-' + crypto.randomUUID(), name, server: null }
    await setServer(null)
    await setSpace(space)
    done(null, space)
  }

  async function submitServer(e: Event) {
    e.preventDefault()
    error = null
    const raw = serverInput.trim()
    // A pairing link: https://host/#pair=CODE
    const m = /^(https?:\/\/[^#]+?)\/?#pair=([^&\s]+)/.exec(raw)
    let url = m ? m[1] : raw
    if (!/^https?:\/\//.test(url)) url = `https://${url}`
    busy = true
    try {
      await setServer(url.replace(/\/+$/, ''))
      space = { ...space, id: space.id === 'local' ? 'remote-' + crypto.randomUUID() : space.id, name: space.name === 'Local Space' ? new URL(url).hostname : space.name, server: url.replace(/\/+$/, '') }
      await setSpace(space)
      needServer = false
      if (m) return await finish((await redeem(decodeURIComponent(m[2]))).token)
      await connect()
    } catch (err) {
      error = `Couldn't use that address: ${(err as Error).message}`
      needServer = true
    } finally {
      busy = false
    }
  }

  onMount(async () => {
    space = await getSpace()
    const configuredServer = await getServer()
    if (configuredServer && !space.server) {
      space = { ...space, id: 'remote-' + crypto.randomUUID(), name: new URL(configuredServer).hostname, server: configuredServer }
      await setSpace(space)
    }
    if (isTauri && !configuredServer) {
      needServer = true
      return
    }
    const m = /[#&]pair=([^&]+)/.exec(location.hash)
    if (m) {
      history.replaceState(null, '', location.pathname)
      busy = true
      try {
        return await finish((await redeem(decodeURIComponent(m[1]))).token)
      } catch (e) {
        error = `Pairing link failed: ${(e as Error).message}`
      } finally {
        busy = false
      }
    }
    await connect()
  })

  async function submit(e: Event) {
    e.preventDefault()
    error = null
    busy = true
    try {
      if (auth?.needs_setup) {
        if (password.length < 8) throw new Error('Use at least 8 characters')
        if (password !== confirm) throw new Error('The passwords do not match')
        await finish((await setup(password, auth.setup_code_required ? code.trim() : null)).token)
      } else {
        await finish((await login(password)).token)
      }
    } catch (err) {
      const m = (err as Error).message
      error = m === 'wrong password' ? 'Wrong password' : m === '429' ? 'Too many attempts — wait a minute' : m
    } finally {
      busy = false
    }
  }
</script>

<main class="auth">
  {#if needServer}
    <form onsubmit={submitServer} data-testid="server-form">
      <h1>Spaces</h1>
      <p>Connect to a remote Space, or keep your notes on this device only.</p>
      <label>Server or pairing link <input bind:value={serverInput} placeholder="https://notes.example.com" inputmode="url" autocapitalize="off" required data-testid="server" /></label>
      <button class="btn primary" disabled={busy} type="submit">Continue</button>
      <button class="btn" type="button" onclick={() => void useLocal()}>Create local Space</button>
      {#if error}<p class="error" role="alert">{error}</p>{/if}
    </form>
  {:else}
    <form onsubmit={submit} data-testid="auth">
      <h1>{space.name}</h1>
      {#if offline}
        <p>Can't reach the server. Check your connection and reload.</p>
        <button class="btn primary" type="button" onclick={() => void useLocal()}>Create local Space</button>
        <button class="btn" type="button" onclick={() => (needServer = true)}>Add remote Space</button>
      {:else if !auth}
        <p class="muted">{busy ? 'Pairing…' : 'Connecting…'}</p>
      {:else if auth.needs_setup}
        <p>Choose the password for this remote Space. You'll use it to sign in new devices.</p>
        {#if auth.setup_code_required}
          <label>Setup code (printed in the server log) <input bind:value={code} autocomplete="one-time-code" required data-testid="setup-code" /></label>
        {/if}
        <label>Password <input type="password" bind:value={password} autocomplete="new-password" required data-testid="password" /></label>
        <label>Confirm password <input type="password" bind:value={confirm} autocomplete="new-password" required data-testid="confirm" /></label>
        <button class="btn primary" disabled={busy} type="submit">Create vault</button>
      {:else}
        <label>Password <input type="password" bind:value={password} autocomplete="current-password" required data-testid="password" /></label>
        <button class="btn primary" disabled={busy} type="submit">Sign in</button>
      {/if}
      {#if error}<p class="error" role="alert">{error}</p>{/if}
    </form>
  {/if}
</main>

<style>
  .auth {
    display: grid;
    place-items: center;
    height: 100%;
    background: var(--bg);
  }
  form {
    width: min(360px, 90vw);
    display: flex;
    flex-direction: column;
    gap: 12px;
  }
  h1 {
    font-size: 22px;
    margin: 0 0 8px;
  }
  label {
    display: flex;
    flex-direction: column;
    gap: 4px;
    font-size: 14px;
  }
  .error {
    color: var(--danger);
  }
</style>
