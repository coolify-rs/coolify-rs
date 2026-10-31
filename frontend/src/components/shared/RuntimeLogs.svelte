<script lang="ts">
  // Port of upstream livewire/project/shared/logs.blade.php (Project\Shared\Logs): the deployment and
  // service pickers around the log viewer. The page around it renders the heading and sidebar.
  import Callout from '../Callout.svelte';
  import Empty from '../Empty.svelte';
  import Listbox from '../Listbox.svelte';
  import LogViewer, { type LogRequest } from '../LogViewer.svelte';
  import { ApiError, request } from '../../lib/api';
  import { resPath, type ResourceKind } from '../../lib/sharedApi';
  import { notify } from '../../lib/toast.svelte';

  let { kind, uuid }: { kind: ResourceKind; uuid: string } = $props();

  let data = $state<any>(null);
  let pullRequestId = $state(new URLSearchParams(location.search).get('pull_request_id') ?? '0');
  let service = $state(new URLSearchParams(location.search).get('service') ?? '');

  const base = $derived(`${resPath(kind, uuid)}/logs`);
  const targets = $derived(data?.targets);
  const sources = $derived<{ container: string; label: string }[]>(targets?.sources ?? []);

  async function load() {
    try {
      const q = new URLSearchParams();
      if (pullRequestId !== '0') q.set('pull_request_id', pullRequestId);
      if (service) q.set('service', service);
      const d = await request<any>('GET', `${base}?${q}`);
      data = d;
      pullRequestId = d.targets.deployment;
      service = d.targets.service;
    } catch (err) {
      notify.failure(err instanceof ApiError ? err.message : String(err));
      data = { total_containers: 0, functional: true, targets: { sources: [], deployments: [], services: [] } };
    }
  }

  $effect(() => {
    load();
  });

  function sync() {
    const q = new URLSearchParams(location.search);
    if (pullRequestId && pullRequestId !== '0') q.set('pull_request_id', pullRequestId);
    else q.delete('pull_request_id');
    if (service) q.set('service', service);
    else q.delete('service');
    const s = q.toString();
    history.replaceState(history.state, '', location.pathname + (s ? `?${s}` : ''));
    load();
  }

  async function fetchLogs(r: LogRequest): Promise<string> {
    const q = new URLSearchParams({ mode: r.mode, lines: String(r.lines), timestamps: r.timestamps ? '1' : '0', containers: sources.map((s) => s.container).join(',') });
    if (r.since) q.set('since', r.since);
    const res = await request<{ logs: string }>('GET', `${base}/read?${q}`);
    return res.logs;
  }
</script>

{#if !data}
  <div class="w-full"><div class="min-h-96 animate-pulse rounded-lg bg-neutral-200/80 dark:bg-white/[0.06]"></div></div>
{:else if !data.functional}
  <Empty size="lg" title="Runtime logs unavailable" description="No functional servers are available, so container logs cannot be loaded." iconName="file-content" />
{:else if data.total_containers === 0}
  {#if data.server_error}
    <Empty size="lg" title="Runtime logs unavailable" description="Could not load containers: {data.server_error}" iconName="file-content" />
  {:else}
    <Empty size="lg" title="Runtime logs unavailable" description="No containers are running, so there are no runtime logs to show." iconName="file-content" />
  {/if}
{:else}
  <div class="runtime-logs-workspace">
    {#if data.server_error}
      <Callout type="warning" title="Could not load containers">{data.server_name}: {data.server_error}</Callout>
    {/if}
    {#if sources.length > 0}
      {#if targets.deployments.length > 1 || targets.services.length > 1}
        <div class="flex shrink-0 flex-col gap-3 sm:flex-row sm:flex-wrap sm:items-end">
          {#if targets.deployments.length > 1}
            <div class="w-full sm:w-60">
              <Listbox id="pullRequestId" label="Deployment" searchable searchPlaceholder="Search deployments" bind:value={pullRequestId} onchange={sync} options={targets.deployments} />
            </div>
          {/if}
          {#if targets.services.length > 1}
            <div class="w-full sm:w-60">
              <Listbox id="service" label="Service" searchable searchPlaceholder="Search services" bind:value={service} onchange={sync} options={targets.services} />
            </div>
          {/if}
        </div>
      {/if}
      {#key targets.stream_key}
        <LogViewer {fetchLogs} containerName={sources[0].container} timezone={data.timezone} collapsible={false} fill />
      {/key}
    {:else}
      <Empty size="lg" title="Runtime logs unavailable" description="No containers are running, so there are no runtime logs to show." iconName="file-content" />
    {/if}
  </div>
{/if}
