<script lang="ts">
  // coolify-rs's own (ADR 0013): the server's Plugins tab, with the team's server panels. The tab only
  // exists while the team has one; otherwise the path goes home like any unknown server page.
  import PluginPanels from '../components/PluginPanels.svelte';
  import ServerFrame from '../components/ServerFrame.svelte';
  import { pluginNav } from '../lib/plugins.svelte';
  import { router } from '../lib/router.svelte';

  let { uuid }: { uuid: string } = $props();

  $effect(() => {
    if (pluginNav.loaded && !pluginNav.of('server').length) router.go('/', true);
  });
</script>

{#if pluginNav.of('server').length}
  <ServerFrame {uuid} active="plugins" title="Plugins">
    {#snippet children()}
      <div class="application-settings-form flex w-full flex-col gap-6">
        <PluginPanels slot="server" resource={uuid} />
      </div>
    {/snippet}
  </ServerFrame>
{/if}
