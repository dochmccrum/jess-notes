<script lang="ts">
  // Spaces (DESIGN §24): the welcome screen on a fresh install ("on this device" or "on a Jess
  // server") and, inside the app, the list of spaces (open, rename, delete), adding one, and
  // moving the open local space to a server. Apps only. Opening, adding and moving restart the app
  // into the space.
  import { onMount } from 'svelte'
  import { isAndroid } from '../lib/platform'
  import {
    addLocalSpace,
    addRemoteSpace,
    deleteSpace,
    isPairingLink,
    listSpaces,
    moveSpaceToServer,
    renameSpace,
    scanPairingCode,
    switchSpace,
    type Space,
    type SpacesState,
  } from '../lib/spaces'

  let { welcome = false, current = null }: { welcome?: boolean; current?: Space | null } = $props()

  let reg: SpacesState | null = $state(null)
  let busy = $state('')
  let error = $state('')
  // Adding.
  let adding: 'local' | 'remote' | null = $state(null)
  let localName = $state('Notes')
  let server = $state('')
  let password = $state('')
  // Managing.
  let renaming: string | null = $state(null)
  let newName = $state('')
  let deleting: Space | null = $state(null)
  let confirmName = $state('')
  let moving = $state(false)
  let moved: Record<string, unknown> | null = $state(null)

  onMount(() => {
    if (!welcome) void refresh()
  })

  async function refresh() {
    try {
      reg = await listSpaces()
    } catch (e) {
      error = (e as Error).message ?? String(e)
    }
  }

  async function run(label: string, f: () => Promise<unknown>) {
    error = ''
    busy = label
    try {
      await f()
    } catch (e) {
      error = typeof e === 'string' ? e : ((e as Error).message ?? String(e))
    } finally {
      busy = ''
    }
  }

  const pairing = $derived(isPairingLink(server))

  async function scan() {
    await run('Scanning…', async () => {
      const code = await scanPairingCode()
      if (code) server = code.trim()
    })
  }

  function submitLocal(e: Event) {
    e.preventDefault()
    void run('Creating the space…', () => addLocalSpace(localName))
  }

  function submitRemote(e: Event) {
    e.preventDefault()
    void run('Signing in…', () => addRemoteSpace(server, pairing ? null : password))
  }

  function submitMove(e: Event) {
    e.preventDefault()
    void run('Moving your notes…', async () => {
      moved = await moveSpaceToServer(server, pairing ? null : password)
    })
  }

  async function doRename(s: Space) {
    await run('', async () => {
      reg = await renameSpace(s.id, newName)
      renaming = null
    })
  }

  async function doDelete() {
    const s = deleting!
    await run('Deleting…', async () => {
      reg = await deleteSpace(s.id)
      deleting = null
      confirmName = ''
    })
  }
</script>

{#snippet remoteForm(onsubmit: (e: Event) => void, action: string)}
  <form {onsubmit} class="form" data-testid="space-remote-form">
    <label>
      Server address or pairing link
      <input bind:value={server} placeholder="https://notes.example.com" inputmode="url" autocapitalize="off" spellcheck="false" required data-testid="space-server" />
    </label>
    {#if isAndroid}
      <button class="btn" type="button" onclick={() => void scan()} disabled={!!busy}>Scan a pairing code</button>
    {/if}
    {#if !pairing}
      <label>
        Password
        <input type="password" bind:value={password} autocomplete="current-password" required data-testid="space-password" />
      </label>
    {:else}
      <p class="small muted">A pairing link signs this device in: no password needed.</p>
    {/if}
    <button class="btn primary" type="submit" disabled={!!busy} data-testid="space-remote-submit">{busy || action}</button>
  </form>
{/snippet}

{#snippet localForm()}
  <form onsubmit={submitLocal} class="form" data-testid="space-local-form">
    <label>
      Name
      <input bind:value={localName} required maxlength="80" data-testid="space-name" />
    </label>
    <p class="small muted">Notes stay on this device, with no server. You can move them to a server later.</p>
    <button class="btn primary" type="submit" disabled={!!busy} data-testid="space-local-submit">{busy || 'Create space'}</button>
  </form>
{/snippet}

{#if welcome}
  <main class="welcome" data-testid="spaces-welcome">
    <div class="col">
      <h1>Jess Notes</h1>
      <p>Where should your notes live?</p>
      <div class="choices">
        <button class="choice" class:on={adding === 'local'} onclick={() => (adding = 'local')} data-testid="choose-local">
          <strong>On this device</strong>
          <span class="small muted">A local space. No account or server needed.</span>
        </button>
        <button class="choice" class:on={adding === 'remote'} onclick={() => (adding = 'remote')} data-testid="choose-remote">
          <strong>On a Jess server</strong>
          <span class="small muted">Synced with your other devices.</span>
        </button>
      </div>
      {#if adding === 'local'}{@render localForm()}{/if}
      {#if adding === 'remote'}{@render remoteForm(submitRemote, 'Connect')}{/if}
      {#if error}<p class="error" role="alert">{error}</p>{/if}
    </div>
  </main>
{:else}
  <div class="manage" data-testid="spaces-manage">
    <ul class="list" aria-label="Spaces">
      {#each reg?.spaces ?? [] as s (s.id)}
        <li class:current={s.id === reg?.active} data-testid="space-row">
          {#if renaming === s.id}
            <form class="rename" onsubmit={(e) => (e.preventDefault(), void doRename(s))}>
              <input bind:value={newName} required maxlength="80" aria-label="New name" />
              <button class="btn small primary" type="submit">Save</button>
              <button class="btn small" type="button" onclick={() => (renaming = null)}>Cancel</button>
            </form>
          {:else}
            <div class="who">
              <strong>{s.name}</strong>
              <span class="small muted">{s.kind === 'local' ? 'On this device' : (s.server ?? '').replace(/^https?:\/\//, '')}</span>
            </div>
            <div class="acts">
              {#if s.id === reg?.active}
                <span class="small muted">Open</span>
              {:else}
                <button class="btn small primary" disabled={!!busy} onclick={() => void run('Opening…', () => switchSpace(s.id))} data-testid="space-open">Open</button>
              {/if}
              <button class="btn small" onclick={() => ((renaming = s.id), (newName = s.name))}>Rename</button>
              {#if s.id !== reg?.active}
                <button class="btn small danger" onclick={() => ((deleting = s), (confirmName = ''))} data-testid="space-delete">Delete…</button>
              {/if}
            </div>
          {/if}
        </li>
      {/each}
    </ul>

    {#if deleting}
      <section class="confirm" role="alert">
        {#if deleting.kind === 'local'}
          <p><strong>{deleting.name}</strong> exists only on this device. Deleting it deletes its notes and attachments for good. Type its name to confirm.</p>
          <input bind:value={confirmName} aria-label="Space name" data-testid="space-delete-name" />
        {:else}
          <p>This removes <strong>{deleting.name}</strong> from this device. The server keeps it, and you can add it again.</p>
        {/if}
        <div class="row">
          <button class="btn danger" disabled={!!busy || (deleting.kind === 'local' && confirmName.trim() !== deleting.name)} onclick={() => void doDelete()} data-testid="space-delete-confirm">Delete</button>
          <button class="btn" onclick={() => (deleting = null)}>Cancel</button>
        </div>
      </section>
    {/if}

    <h3>Add a space</h3>
    <div class="row">
      <button class="btn" class:primary={adding === 'local'} onclick={() => (adding = 'local')} data-testid="add-local">On this device</button>
      <button class="btn" class:primary={adding === 'remote'} onclick={() => ((adding = 'remote'), (moving = false))} data-testid="add-remote">On a Jess server</button>
    </div>
    {#if adding === 'local'}{@render localForm()}{/if}
    {#if adding === 'remote'}{@render remoteForm(submitRemote, 'Connect')}{/if}

    {#if current?.kind === 'local'}
      <h3>Move to a server</h3>
      {#if moved}
        <p data-testid="space-moved">Moved. Opening it from the server… The copy on this device stays as “{current.name} (moved)”.</p>
      {:else if moving}
        <p class="small muted">Copies this space's notes and attachments to a Jess server, then opens it from there, synced with your other devices. This device keeps its copy until you delete it.</p>
        {@render remoteForm(submitMove, 'Move')}
      {:else}
        <button class="btn" onclick={() => ((moving = true), (adding = null))} data-testid="space-move">Move “{current.name}” to a server…</button>
      {/if}
    {/if}
    {#if error}<p class="error" role="alert">{error}</p>{/if}
  </div>
{/if}

<style>
  .welcome {
    display: grid;
    place-items: center;
    min-height: 100%;
    background: var(--bg);
    padding: 24px 16px;
    box-sizing: border-box;
  }
  .col {
    width: min(420px, 100%);
    display: flex;
    flex-direction: column;
    gap: 12px;
  }
  h1 {
    font-size: 22px;
    margin: 0;
  }
  h3 {
    font-size: 14px;
    margin: 18px 0 8px;
  }
  .choices {
    display: grid;
    gap: 8px;
  }
  .choice {
    display: flex;
    flex-direction: column;
    gap: 2px;
    text-align: left;
    padding: 12px 14px;
    border: 1px solid var(--border);
    border-radius: 8px;
    background: var(--bg-2);
    color: inherit;
    cursor: pointer;
  }
  .choice.on {
    border-color: var(--accent);
    box-shadow: 0 0 0 1px var(--accent);
  }
  .form {
    display: flex;
    flex-direction: column;
    gap: 10px;
    margin-top: 4px;
  }
  label {
    display: flex;
    flex-direction: column;
    gap: 4px;
    font-size: 14px;
  }
  .list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .list li {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    padding: 8px 10px;
    border: 1px solid var(--border);
    border-radius: 6px;
  }
  .list li.current {
    border-color: var(--accent);
  }
  .who {
    display: flex;
    flex-direction: column;
    min-width: 0;
  }
  .who span {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .acts,
  .row,
  .rename {
    display: flex;
    gap: 6px;
    flex-wrap: wrap;
    align-items: center;
  }
  .rename input {
    flex: 1;
    min-width: 120px;
  }
  .confirm {
    margin-top: 10px;
    padding: 10px;
    border: 1px solid var(--danger);
    border-radius: 6px;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .error {
    color: var(--danger);
  }
</style>
