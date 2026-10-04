<script lang="ts">
  import Modal from './Modal.svelte'
  import type { AppState } from '../stores/app.svelte'
  import { changePassword, createPairing, getServer, getToken, listDevices, revokeDevice, setToken } from '../lib/auth'
  import { bindingsFor, get as getCommand } from '../lib/commands'
  import { applyTheme } from '../lib/theme'
  import { requestErase } from '../lib/erase'

  let { app, logout }: { app: AppState; logout(): void } = $props()
  let devices: { id: string; name: string; last_seen: number | null; revoked_at: number | null }[] = $state([])
  let pair: { link: string; expires: number } | null = $state(null)
  let quarantine: { opId: number; reason: string; op: string }[] = $state([])
  let token: string | null = null

  $effect(() => {
    void (async () => {
      token = await getToken()
      if (token) devices = await listDevices(token).catch(() => [])
      quarantine = (await app.backend.quarantine()) as typeof quarantine
    })()
  })

  function save() {
    app.backend.entries.showAllAttachments = app.device.showAllAttachments
    app.saveDevice()
    app.version++
    applyTheme(app.device.theme)
    app.backend.setOfflineMode(app.device.offlineAttachments)
  }

  // A local space has no server other devices can reach (DESIGN §24): no pairing, and it's
  // deleted from the Spaces dialog rather than erased.
  const local = $derived(app.space?.kind === 'local')

  async function makePair() {
    if (!token) return
    const r = await createPairing(token)
    pair = { link: `${(await getServer()) ?? location.origin}/#pair=${r.code}`, expires: r.expires_at }
  }

  let pw = $state({ current: '', next: '', again: '' })
  let pwMsg: { ok: boolean; text: string } | null = $state(null)
  let pwBusy = $state(false)
  async function savePassword(e: SubmitEvent) {
    e.preventDefault()
    if (!token) return
    if (pw.next.length < 8) return void (pwMsg = { ok: false, text: 'The new password needs at least 8 characters.' })
    if (pw.next !== pw.again) return void (pwMsg = { ok: false, text: 'The new passwords don’t match.' })
    pwBusy = true
    pwMsg = null
    try {
      await changePassword(token, pw.current, pw.next)
      pw = { current: '', next: '', again: '' }
      pwMsg = { ok: true, text: 'Password changed. Signed-in devices stay signed in.' }
    } catch (err) {
      const m = (err as Error).message
      pwMsg = { ok: false, text: m === '429' ? 'Too many attempts: try again in a minute.' : m === 'wrong password' ? 'The current password is wrong.' : `Couldn’t change it: ${m}` }
    } finally {
      pwBusy = false
    }
  }

  async function revoke(id: string) {
    if (!token) return
    await revokeDevice(token, id)
    devices = await listDevices(token)
  }

  let pending = $state(0)
  $effect(() => app.backend.sync.subscribe((s) => (pending = s.pending ?? 0)))
  let erasing = $state(false)

  async function doLogout() {
    await setToken(null)
    logout()
  }
</script>

<Modal title="Settings" close={() => (app.overlay = null)} wide>
  <section>
    <h3>Sidebar</h3>
    <label>
      Mode
      <select bind:value={app.device.sidebarMode} onchange={save} data-testid="sidebar-mode">
        <option value="pinned">Pinned (always visible)</option>
        <option value="shortcut">Shortcut (slides in with {navigator.platform.includes('Mac') ? '⌘' : 'Ctrl'}+\)</option>
        <option value="hover">Hover (reveal at the left edge)</option>
      </select>
    </label>
    <label class="check"><input type="checkbox" bind:checked={app.device.showAllAttachments} onchange={save} /> Show all attachments in file tree</label>
  </section>
  <section>
    <h3>Appearance</h3>
    <label>
      Theme
      <select bind:value={app.device.theme} onchange={save}>
        <option value="system">Follow system</option>
        <option value="light">Light</option>
        <option value="dark">Dark</option>
      </select>
    </label>
  </section>
  <section>
    <h3>Attachments on this device</h3>
    <label>
      Offline attachments
      <select bind:value={app.device.offlineAttachments} onchange={save}>
        <option value="everything">Keep everything offline</option>
        <option value="on-demand">Download on demand</option>
      </select>
    </label>
  </section>
  {#if local}
  <section>
    <h3>Devices</h3>
    <p class="small muted">“{app.space?.name}” is stored on this device only. To use it on your other devices, move it to a Jess server.</p>
    <button class="btn" onclick={() => (app.overlay = 'spaces')} data-testid="settings-spaces">Spaces…</button>
  </section>
  {:else}
  <section>
    <h3>Devices</h3>
    <ul class="devices">
      {#each devices as d (d.id)}
        <li>
          <span>{d.name}</span>
          <span class="muted small">{d.revoked_at ? 'revoked' : d.last_seen ? `last seen ${new Date(d.last_seen).toLocaleString()}` : ''}</span>
          {#if !d.revoked_at}<button class="btn" onclick={() => void revoke(d.id)}>Revoke</button>{/if}
        </li>
      {/each}
    </ul>
    <button class="btn" onclick={() => void makePair()}>Pair a device…</button>
    {#if pair}
      <p class="small">Open this link on the other device within 10 minutes (single use):</p>
      <input type="text" readonly value={pair.link} onclick={(e) => (e.target as HTMLInputElement).select()} class="link" />
      <p class="small">Or scan it with the Jess app on the other device (Add a space → On a Jess server → Scan):</p>
      {#await import('uqr') then { renderSVG }}
        <img class="qr" alt="Pairing QR code" data-testid="pair-qr" src={`data:image/svg+xml;utf8,${encodeURIComponent(renderSVG(pair.link, { border: 2 }))}`} />
      {/await}
    {/if}
  </section>
  <section>
    <h3>Password</h3>
    <form class="password" onsubmit={savePassword} data-testid="change-password">
      <input type="password" autocomplete="current-password" placeholder="Current password" bind:value={pw.current} aria-label="Current password" />
      <input type="password" autocomplete="new-password" placeholder="New password (8+ characters)" bind:value={pw.next} aria-label="New password" />
      <input type="password" autocomplete="new-password" placeholder="New password again" bind:value={pw.again} aria-label="New password again" />
      <button class="btn" type="submit" disabled={!pw.current || !pw.next || pwBusy}>{pwBusy ? 'Changing…' : 'Change password'}</button>
    </form>
    {#if pwMsg}<p class="small" class:muted={pwMsg.ok} role="status">{pwMsg.text}</p>{/if}
  </section>
  {/if}
  {#if quarantine.length}
    <section>
      <h3>Rejected changes</h3>
      <p class="small muted">The server refused these changes. They are kept here and never discarded.</p>
      <ul>
        {#each quarantine as q (q.opId)}
          <li class="small"><strong>{q.reason}</strong> <code>{q.op.slice(0, 120)}</code></li>
        {/each}
      </ul>
    </section>
  {/if}
  <section>
    <h3>Keyboard shortcuts</h3>
    <table class="keys">
      <tbody>
        {#each bindingsFor() as [k, id]}
          <tr><td><kbd>{k.replace('Meta', '⌘').replace(/-/g, '+')}</kbd></td><td>{getCommand(id)?.title ?? id}</td></tr>
        {/each}
      </tbody>
    </table>
  </section>
  <section class="row">
    <button class="btn" onclick={() => (app.overlay = 'trash')}>Open trash</button>
    <button class="btn" onclick={() => (app.overlay = 'import')}>Import / export…</button>
    <button class="btn" onclick={() => (app.overlay = 'attachments')}>Attachments…</button>
    {#if !local}
      <button class="btn danger" onclick={() => void doLogout()}>Log out of this device</button>
      <button class="btn danger" onclick={() => (erasing = true)} data-testid="erase">Erase this device's local copy…</button>
    {/if}
  </section>
  {#if erasing}
    <section class="erase" role="alert">
      <p>This deletes every note and attachment stored on this device and signs it out. The vault on the server is not touched; sign in again to download it.</p>
      {#if pending}
        <p class="warn"><strong>{pending} change{pending === 1 ? ' has' : 's have'} not reached the server yet and will be lost.</strong> Export first (Import / export → Export as .zip) if you need them.</p>
      {/if}
      <div class="row">
        <button class="btn" onclick={() => (erasing = false)}>Cancel</button>
        <button class="btn danger" onclick={() => requestErase()} data-testid="erase-confirm">Erase and reload</button>
      </div>
    </section>
  {/if}
</Modal>

<style>
  section {
    margin-bottom: 18px;
  }
  h3 {
    font-size: 13px;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--fg-2);
    margin: 0 0 8px;
  }
  label {
    display: flex;
    flex-direction: column;
    gap: 4px;
    margin-bottom: 8px;
    font-size: 14px;
  }
  label.check {
    flex-direction: row;
    align-items: center;
    gap: 8px;
  }
  .password {
    display: flex;
    flex-direction: column;
    gap: 6px;
    max-width: 320px;
  }
  .devices {
    list-style: none;
    padding: 0;
  }
  .devices li {
    display: flex;
    gap: 8px;
    align-items: center;
    padding: 4px 0;
  }
  .devices li span:first-child {
    flex: 1;
  }
  .small {
    font-size: 12px;
  }
  .link {
    width: 100%;
    font-family: var(--mono);
    font-size: 12px;
  }
  .keys td {
    padding: 2px 12px 2px 0;
    font-size: 13px;
  }
  .row {
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
  }
  .danger {
    color: var(--danger);
  }
  .erase {
    border: 1px solid var(--danger);
    border-radius: var(--radius);
    padding: 12px;
  }
  .warn {
    color: var(--danger);
  }
  .qr {
    width: 200px;
    height: 200px;
    background: #fff;
    border-radius: 6px;
    image-rendering: pixelated;
  }
</style>
