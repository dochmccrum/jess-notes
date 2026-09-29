<script lang="ts">
  import Sidebar from '../src/components/Sidebar.svelte'
  import FileTree from '../src/components/FileTree.svelte'
  import type { AppState } from '../src/stores/app.svelte'
  let { app, tree = false }: { app: AppState; tree?: boolean } = $props()
</script>

<Sidebar {app}>
  {#if tree}<FileTree {app} />{:else}<p>content</p>{/if}
</Sidebar>
