<script lang="ts" generics="T">
  // Fixed-row-height virtual list (DESIGN §11.3). Renders only the visible slice.
  import type { Snippet } from 'svelte'

  let {
    items,
    rowHeight,
    row,
    overscan = 8,
    scrollTo = null,
    ...rest
  }: { items: T[]; rowHeight: number; row: Snippet<[T, number]>; overscan?: number; scrollTo?: number | null; [k: string]: unknown } = $props()

  let el: HTMLDivElement | undefined = $state()
  let top = $state(0)
  let height = $state(600)

  const start = $derived(Math.max(0, Math.floor(top / rowHeight) - overscan))
  const end = $derived(Math.min(items.length, Math.ceil((top + height) / rowHeight) + overscan))
  const visible = $derived(items.slice(start, end))

  $effect(() => {
    if (!el) return
    const ro = new ResizeObserver(() => (height = el!.clientHeight || 600))
    ro.observe(el)
    return () => ro.disconnect()
  })

  $effect(() => {
    if (scrollTo == null || !el) return
    const y = scrollTo * rowHeight
    if (y < el.scrollTop) el.scrollTop = y
    else if (y + rowHeight > el.scrollTop + el.clientHeight) el.scrollTop = y + rowHeight - el.clientHeight
  })
</script>

<div class="vlist" bind:this={el} onscroll={() => (top = el!.scrollTop)} {...rest}>
  <div class="spacer" style:height="{items.length * rowHeight}px">
    {#each visible as item, i (start + i)}
      <div class="vrow" style:transform="translateY({(start + i) * rowHeight}px)" style:height="{rowHeight}px">
        {@render row(item, start + i)}
      </div>
    {/each}
  </div>
</div>

<style>
  .vlist {
    overflow-y: auto;
    position: relative;
    height: 100%;
    contain: strict;
  }
  .spacer {
    position: relative;
    width: 100%;
  }
  .vrow {
    position: absolute;
    left: 0;
    right: 0;
    top: 0;
  }
</style>
