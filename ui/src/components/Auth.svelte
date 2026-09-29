<script lang="ts">
  // First-run setup, login and pairing-link redemption (DESIGN §14).
  import { onMount } from 'svelte'
  import { authState, login, redeem, setToken, setup, type AuthState } from '../lib/auth'

  let { done }: { done(token: string): void } = $props()
  let auth = $state<AuthState | null>(null)
  let password = $state('')
  let confirm = $state('')
  let code = $state('')
  let error: string | null = $state(null)
  let busy = $state(false)
  let offline = $state(false)

  async function finish(t: string) {
    await setToken(t)
    done(t)
  }

  onMount(async () => {
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
    try {
      auth = await authState()
    } catch {
      offline = true
    }
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
  <form onsubmit={submit} data-testid="auth">
    <h1>Jess Notes</h1>
    {#if offline}
      <p>Can't reach the server. Check your connection and reload.</p>
    {:else if !auth}
      <p class="muted">{busy ? 'Pairing…' : 'Connecting…'}</p>
    {:else if auth.needs_setup}
      <p>Choose the password for this vault. You'll use it to sign in new devices.</p>
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
