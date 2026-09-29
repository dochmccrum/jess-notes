<script lang="ts">
  import Modal from './Modal.svelte'
  import type { AppState } from '../stores/app.svelte'
  import { createPairing, getToken, listDevices, revokeDevice, setToken } from '../lib/auth'
  import { bindingsFor, get as getCommand } from '../lib/commands'

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
  }

  export function applyTheme(t: string) {
    if (t === 'system') document.documentElement.removeAttribute('data-theme')
    else document.documentElement.dataset.theme = t
  }

  async function makePair() {
    if (!token) return
    const r = await createPairing(token)
    pair = { link: `${location.origin}/#pair=${r.code}`, expires: r.expires_at }
  }

  async function revoke(id: string) {
    if (!token) return
    await revokeDevice(token, id)
    devices = await listDevices(token)
  }

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
    {/if}
  </section>
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
    <button class="btn danger" onclick={() => void doLogout()}>Log out of this device</button>
  </section>
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
</style>
