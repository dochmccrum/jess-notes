<script lang="ts" module>
  export interface PickItem {
    id: string
    label: string
    detail?: string
    hint?: string
  }
</script>

<script lang="ts">
  // Shared modal picker (quick switcher, command palette, move-to): ARIA combobox + listbox.
  import { onMount, tick } from 'svelte'

  let {
    placeholder,
    query = $bindable(''),
    items,
    pick,
    close,
    footer = '',
  }: { placeholder: string; query?: string; items: PickItem[]; pick(item: PickItem, e: KeyboardEvent | MouseEvent): void; close(): void; footer?: string } = $props()

  let dlg: HTMLDialogElement | undefined = $state()
  let input: HTMLInputElement | undefined = $state()
  let sel = $state(0)
  let listEl: HTMLUListElement | undefined = $state()

  onMount(() => {
    dlg!.showModal()
    input!.focus()
  })

  $effect(() => {
    void items
    sel = 0
  })

  async function key(e: KeyboardEvent) {
    if (e.key === 'ArrowDown') sel = Math.min(items.length - 1, sel + 1)
    else if (e.key === 'ArrowUp') sel = Math.max(0, sel - 1)
    else if (e.key === 'Enter') {
      const it = items[sel]
      if (it) pick(it, e)
    } else if (e.key === 'Escape') close()
    else return
    e.preventDefault()
    await tick()
    listEl?.querySelector('[aria-selected="true"]')?.scrollIntoView({ block: 'nearest' })
  }
</script>

<dialog bind:this={dlg} class="picker" oncancel={(e) => { e.preventDefault(); close() }} onclick={(e) => e.target === dlg && close()}>
  <input
    bind:this={input}
    bind:value={query}
    type="text"
    {placeholder}
    role="combobox"
    aria-expanded="true"
    aria-controls="picker-list"
    aria-activedescendant={items[sel] ? `pick-${sel}` : undefined}
    autocomplete="off"
    spellcheck="false"
    onkeydown={key}
  />
  <ul id="picker-list" role="listbox" bind:this={listEl}>
    {#each items as it, i (it.id + i)}
      <li id="pick-{i}" role="option" aria-selected={i === sel} onmousemove={() => (sel = i)} onclick={(e) => pick(it, e)}>
        <span class="label">{it.label}</span>
        {#if it.detail}<span class="detail">{it.detail}</span>{/if}
        {#if it.hint}<kbd>{it.hint}</kbd>{/if}
      </li>
    {:else}
      <li class="none muted">No matches</li>
    {/each}
  </ul>
  {#if footer}<div class="footer muted">{footer}</div>{/if}
</dialog>

<style>
  .picker {
    width: min(620px, 94vw);
    margin-top: 12vh;
  }
  input {
    width: 100%;
    border: 0;
    border-bottom: 1px solid var(--border);
    border-radius: 0;
    padding: 14px 16px;
    font-size: 16px;
    outline: none;
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 4px;
    max-height: 50vh;
    overflow-y: auto;
  }
  li {
    display: flex;
    gap: 10px;
    align-items: baseline;
    padding: 7px 12px;
    border-radius: 6px;
    cursor: pointer;
  }
  li[aria-selected='true'] {
    background: var(--accent);
    color: var(--accent-fg);
  }
  li[aria-selected='true'] .detail {
    color: inherit;
    opacity: 0.8;
  }
  .label {
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .detail {
    color: var(--fg-3);
    font-size: 13px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    flex: 1;
  }
  kbd {
    margin-left: auto;
    font-size: 12px;
    opacity: 0.7;
  }
  .none {
    cursor: default;
  }
  .footer {
    font-size: 12px;
    padding: 6px 14px;
    border-top: 1px solid var(--border);
  }
  @media (pointer: coarse) {
    li {
      min-height: 44px;
      align-items: center;
    }
  }
</style>
