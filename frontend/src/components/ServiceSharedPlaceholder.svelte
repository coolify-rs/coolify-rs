<script lang="ts">
  // Placeholder for a `Project/Shared/*` section on a service page. The shared
  // components are ported on their own branch and connected at merge.
  import Unported from '../pages/Unported.svelte';

  let { section }: { section: string } = $props();
</script>

<div data-shared-section={section}>
  <Unported />
</div>
