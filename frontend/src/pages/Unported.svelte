<script lang="ts">
  // Shown inside the normal chrome for upstream pages coolify-rs does not
  // implement yet, so every link in the ported navigation goes somewhere.
  import Empty from '../components/Empty.svelte';
  import { link } from '../lib/router.svelte';

  let {
    title = 'Not available in coolify-rs yet',
    description = 'This page exists in Coolify but has not been ported to coolify-rs. Use Coolify for it for now.',
    iconName = 'info-circle',
  }: { title?: string; description?: string; iconName?: string } = $props();
</script>

<div class="application-settings-form w-full">
  <Empty {title} {description} {iconName} size="lg">
    {#snippet contents()}
      <a href="/" use:link class="button">Go to dashboard</a>
    {/snippet}
  </Empty>
</div>
