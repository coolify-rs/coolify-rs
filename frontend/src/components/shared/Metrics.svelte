<script lang="ts">
  // Port of the states of upstream livewire/project/shared/metrics.blade.php before charts are shown.
  // The charts read Sentinel on the server, which needs a running Sentinel to verify; they are not
  // ported, so that state keeps the frame and says so.
  import Empty from '../Empty.svelte';
  import Reicon from '../Reicon.svelte';
  import SettingsSection from '../SettingsSection.svelte';
  import StatusBadge from '../StatusBadge.svelte';
  import { link } from '../../lib/router.svelte';
  import { request } from '../../lib/api';
  import { resPath, type ResourceKind } from '../../lib/sharedApi';

  let { kind, uuid }: { kind: ResourceKind; uuid: string } = $props();
  let d = $state<any>(null);
  $effect(() => {
    request<any>('GET', `${resPath(kind, uuid)}/metrics`).then((r) => (d = r));
  });
</script>

{#if d}
  <div class="flex flex-col gap-6">
    {#if d.is_application && d.build_pack === 'dockercompose'}
      <SettingsSection id="metrics-overview-section" title="Metrics" description="Inspect CPU and memory usage for this application.">
        <Empty size="sm" title="Metrics unavailable" description="Container metrics are not currently available for Docker Compose applications." iconName="dashboard" />
      </SettingsSection>
    {:else if !d.metrics_enabled}
      <SettingsSection id="metrics-overview-section" title="Metrics" description="Inspect CPU and memory usage for this application.">
        {#snippet actions()}
          <a use:link class="button" href="/server/{d.server_uuid}/metrics">
            Server metrics
            <Reicon name="external-link" class="size-3" />
          </a>
        {/snippet}
        <Empty size="sm" title="Metrics are not enabled" description="Enable metrics for this server before collecting application usage data." iconName="dashboard" />
      </SettingsSection>
    {:else if !d.running}
      <SettingsSection id="metrics-overview-section" title="Metrics" description="Inspect CPU and memory usage for this application.">
        {#snippet actions()}
          <StatusBadge label="Not running" type="neutral" />
        {/snippet}
        <Empty size="sm" title="Container is not running" description="Start the application to begin collecting CPU and memory metrics." iconName="dashboard" />
      </SettingsSection>
    {:else}
      <SettingsSection id="metrics-overview-section" title="Metrics" description="Inspect recent CPU and memory usage reported by Sentinel.">
        <Empty size="sm" title="Resource charts are not available in coolify-rs yet" iconName="dashboard" />
      </SettingsSection>
    {/if}
  </div>
{/if}
