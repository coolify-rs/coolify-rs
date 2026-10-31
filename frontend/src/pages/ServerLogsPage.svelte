<script lang="ts">
  // Port of upstream livewire/server/proxy/logs.blade.php and sentinel/logs.blade.php.
  // The log panel covers lines, timestamps, refresh and follow by polling; the
  // search, filter, copy and download controls of `project.shared.get-logs` are not ported.
  import { onDestroy } from 'svelte';
  import Empty from '../components/Empty.svelte';
  import FormInput from '../components/FormInput.svelte';
  import Checkbox from '../components/Checkbox.svelte';
  import ServerFrame from '../components/ServerFrame.svelte';
  import SettingsSection from '../components/SettingsSection.svelte';
  import StatusBadge from '../components/StatusBadge.svelte';
  import { get, failure } from '../lib/serverApi';

  let { uuid, kind = 'proxy' }: { uuid: string; kind?: 'proxy' | 'sentinel' } = $props();

  let logs = $state('');
  let lines = $state('100');
  let timestamps = $state(false);
  let follow = $state(false);
  let loading = $state(false);
  let timer: ReturnType<typeof setInterval> | undefined;
  const container = $derived(kind === 'proxy' ? 'coolify-proxy' : 'coolify-sentinel');

  async function load() {
    loading = true;
    try {
      const r = await get<{ logs: string }>(`/server-page/${encodeURIComponent(uuid)}/logs/${container}?lines=${encodeURIComponent(lines)}&timestamps=${timestamps ? 1 : 0}`);
      logs = r.logs;
    } catch (e) {
      failure(e);
    } finally {
      loading = false;
    }
  }
  function toggleFollow() {
    clearInterval(timer);
    if (follow) timer = setInterval(load, 2000);
  }
  onDestroy(() => clearInterval(timer));
  const headline = (s: string) => (s || 'unknown').replace(/[_-]+/g, ' ').replace(/\b\w/g, (c) => c.toUpperCase());
</script>

<ServerFrame {uuid} active={kind} sub="logs" title={kind === 'proxy' ? 'Proxy Logs' : 'Sentinel Logs'} onloaded={(f) => f.functional && load()}>
  {#snippet children(f)}
    <div class="application-settings-form w-full">
      {#if f.functional}
        <SettingsSection
          title={kind === 'proxy' ? 'Proxy logs' : 'Sentinel logs'}
          description={kind === 'proxy'
            ? 'Search, filter, follow, copy, or download recent output from the Coolify proxy container.'
            : 'Search, filter, follow, copy, or download recent output from the Coolify Sentinel container.'}
          flush
        >
          {#snippet actions()}
            {#if kind === 'proxy'}
              <StatusBadge label={headline(f.proxy_status ?? '')} type={(f.proxy_status ?? '').includes('running') ? 'success' : 'neutral'} />
            {/if}
          {/snippet}
          <div class="settings-log-panel">
            <div class="flex flex-wrap items-end gap-3 border-b border-neutral-200 p-3 dark:border-white/[0.08]">
              <div class="w-28"><FormInput id="lines" label="Lines" type="number" bind:value={lines} /></div>
              <Checkbox id="timestamps" label="Timestamps" bind:checked={timestamps} onchange={load} />
              <Checkbox id="follow" label="Follow logs" bind:checked={follow} onchange={toggleFollow} />
              <button type="button" class="button" disabled={loading} onclick={load}>Refresh</button>
            </div>
            <pre class="max-h-[60vh] overflow-auto bg-neutral-950 p-4 font-mono text-xs leading-5 whitespace-pre-wrap text-neutral-300">{logs || 'No logs yet.'}</pre>
          </div>
        </SettingsSection>
      {:else}
        <SettingsSection title={kind === 'proxy' ? 'Proxy logs' : 'Sentinel logs'}>
          <Empty size="sm" title="Server validation required" description="Validate this server before reading its logs." iconName="servers" />
        </SettingsSection>
      {/if}
    </div>
  {/snippet}
</ServerFrame>
