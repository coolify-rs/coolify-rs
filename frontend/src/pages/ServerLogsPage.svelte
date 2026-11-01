<script lang="ts">
  import Forbidden from '../components/Forbidden.svelte';
  import { session } from '../lib/session.svelte';
  // Port of upstream livewire/server/proxy/logs.blade.php and sentinel/logs.blade.php.
  import Callout from '../components/Callout.svelte';
  import Empty from '../components/Empty.svelte';
  import LogViewer, { type LogRequest } from '../components/LogViewer.svelte';
  import ServerFrame from '../components/ServerFrame.svelte';
  import SettingsSection from '../components/SettingsSection.svelte';
  import StatusBadge from '../components/StatusBadge.svelte';
  import { get } from '../lib/serverApi';

  let { uuid, kind = 'proxy' }: { uuid: string; kind?: 'proxy' | 'sentinel' } = $props();

  const container = $derived(kind === 'proxy' ? 'coolify-proxy' : 'coolify-sentinel');
  const headline = (s: string) => (s || 'unknown').replace(/[_-]+/g, ' ').replace(/\b\w/g, (c) => c.toUpperCase());

  async function fetchLogs(r: LogRequest): Promise<string> {
    const query = new URLSearchParams({ mode: r.mode, lines: String(r.lines), timestamps: r.timestamps ? '1' : '0' });
    if (r.since) query.set('since', r.since);
    const res = await get<{ logs: string }>(`/server-page/${encodeURIComponent(uuid)}/logs/${container}?${query}`);
    return res.logs;
  }
  const sentinelBadge: Record<string, [string, 'neutral' | 'success' | 'warning']> = {
    waiting: ['Waiting for first report', 'neutral'],
    in_sync: ['In sync', 'success'],
    out_of_sync: ['Out of sync', 'warning'],
  };
</script>

<ServerFrame {uuid} active={kind} sub="logs" title={kind === 'proxy' ? 'Proxy Logs' : 'Sentinel Logs'}>
  {#snippet children(f)}
    <div class="application-settings-form w-full">
      {#if kind === 'sentinel' && !f.sentinel_enabled}
        <SettingsSection title="Sentinel logs" description="Search, filter, follow, copy, or download recent output from the Sentinel container." flush class="logs-settings-section">
          <Empty size="sm" title="Sentinel is unavailable" description="Sentinel does not run on build or Swarm servers." iconName="dashboard" />
        </SettingsSection>
      {:else}
        <SettingsSection
          title={kind === 'proxy' ? 'Proxy logs' : 'Sentinel logs'}
          description={kind === 'proxy'
            ? 'Search, filter, follow, copy, or download recent output from the coolify-rs proxy container.'
            : 'Search, filter, follow, copy, or download recent output from the Sentinel container.'}
          flush
          class="logs-settings-section"
        >
          {#snippet actions()}
            {#if kind === 'proxy'}
              <StatusBadge label={headline(f.proxy_status ?? '')} type={(f.proxy_status ?? '').includes('running') ? 'success' : 'neutral'} class="logs-section-status-badge" />
            {:else}
              <StatusBadge label={sentinelBadge[f.sentinel_status][0]} type={sentinelBadge[f.sentinel_status][1]} class="logs-section-status-badge" />
            {/if}
          {/snippet}
          <div class="settings-log-panel">
            {#if !session.isAdmin}
              <Callout type="info" title="Hidden (only admins can view)">Container logs can contain secrets.</Callout>
            {:else}
            <LogViewer {fetchLogs} containerName={container} displayName={kind === 'proxy' ? 'coolify-rs Proxy' : 'Sentinel'} collapsible={false} />
            {/if}
          </div>
        </SettingsSection>
      {/if}
    </div>
  {/snippet}
</ServerFrame>

{#if kind === 'sentinel' && !session.isAdmin}
  <Forbidden />
{/if}
