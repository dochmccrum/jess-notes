<script lang="ts" module>
  export interface MenuItem {
    label: string
    run(): void
    danger?: boolean
  }
</script>

<script lang="ts">
  import { onMount } from 'svelte'

  let { x, y, items, close }: { x: number; y: number; items: MenuItem[]; close(): void } = $props()
  let menu: HTMLDivElement | undefined = $state()
  let pos = $state({ left: 0, top: 0 })

  onMount(() => {
    const r = menu!.getBoundingClientRect()
    pos = { left: Math.min(x, innerWidth - r.width - 8), top: Math.min(y, innerHeight - r.height - 8) }
    ;(menu!.querySelector('button') as HTMLButtonElement | null)?.focus()
    const onDown = (e: PointerEvent) => {
      if (!menu!.contains(e.target as Node)) close()
    }
    setTimeout(() => addEventListener('pointerdown', onDown), 0)
    return () => removeEventListener('pointerdown', onDown)
  })

  function key(e: KeyboardEvent) {
    const btns = [...menu!.querySelectorAll('button')] as HTMLButtonElement[]
    const i = btns.indexOf(document.activeElement as HTMLButtonElement)
    if (e.key === 'Escape') close()
    else if (e.key === 'ArrowDown') btns[(i + 1) % btns.length]?.focus()
    else if (e.key === 'ArrowUp') btns[(i - 1 + btns.length) % btns.length]?.focus()
    else return
    e.preventDefault()
  }
</script>

<div class="menu" role="menu" tabindex="-1" bind:this={menu} style:left="{pos.left}px" style:top="{pos.top}px" onkeydown={key}>
  {#each items as it}
    <button role="menuitem" class:danger={it.danger} onclick={() => { close(); it.run() }}>{it.label}</button>
  {/each}
</div>

<style>
  .menu {
    position: fixed;
    z-index: 100;
    min-width: 190px;
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: 8px;
    box-shadow: var(--shadow);
    padding: 4px;
    display: flex;
    flex-direction: column;
  }
  button {
    text-align: left;
    border: 0;
    background: transparent;
    padding: 6px 10px;
    border-radius: 5px;
    cursor: pointer;
    min-height: 32px;
  }
  button:hover,
  button:focus-visible {
    background: var(--accent);
    color: var(--accent-fg);
    outline: none;
  }
  .danger {
    color: var(--danger);
  }
  @media (pointer: coarse) {
    button {
      min-height: 44px;
    }
  }
</style>
