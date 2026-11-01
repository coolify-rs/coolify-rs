<script lang="ts">
  // Port of upstream livewire/server/resources.blade.php.
  import Reicon from '../components/Reicon.svelte';
  import ViewToggle from '../components/ViewToggle.svelte';
  import Empty from '../components/Empty.svelte';
  import Pager from '../components/Pager.svelte';
  import ResourceLogo from '../components/ResourceLogo.svelte';
  import ServerFrame from '../components/ServerFrame.svelte';
  import SettingsSection from '../components/SettingsSection.svelte';
  import StatusBadge from '../components/StatusBadge.svelte';
  import { get, post, failure } from '../lib/serverApi';
  import { link } from '../lib/router.svelte';
  import { strnatcmp } from '../lib/natsort';
  import { session } from '../lib/session.svelte';
  import { notify } from '../lib/toast.svelte';

  let { uuid }: { uuid: string } = $props();
  type Resource = {
    uuid: string;
    name: string;
    type: string;
    link: string | null;
    project: string | null;
    environment: string | null;
    status: string | null;
    restart_limit?: boolean;
    restart_count?: number;
    service_type?: string | null;
    application?: { buildPack: string | null; sourceType: string | null; fqdn: string | null; basicAuth: boolean };
  };
  type Container = { ID: string; Names: string; Image: string; State: string };

  let tab = $state<'managed' | 'unmanaged'>('managed');
  let resources = $state<Resource[]>([]);
  let containers = $state<Container[]>([]);
  let search = $state('');
  let page = $state(1);
  let pageSize = $state(10);
  let loading = $state<'managed' | 'unmanaged' | null>(null);
  const base = $derived(`/server-page/${encodeURIComponent(uuid)}/resources`);
  const canEdit = $derived(session.isAdmin);

  const headline = (s: string) =>
    s
      .replace(/([a-z])([A-Z])/g, '$1 $2')
      .replace(/[_-]+/g, ' ')
      .trim()
      .split(/\s+/)
      .map((w) => w.charAt(0).toUpperCase() + w.slice(1))
      .join(' ');

  async function loadManaged() {
    if (tab !== 'managed') {
      search = '';
      page = 1;
    }
    tab = 'managed';
    loading = 'managed';
    try {
      resources = (await get<{ resources: Resource[] }>(base)).resources;
    } catch (e) {
      failure(e);
    } finally {
      loading = null;
    }
  }
  async function loadUnmanaged() {
    if (tab !== 'unmanaged') {
      search = '';
      page = 1;
    }
    tab = 'unmanaged';
    loading = 'unmanaged';
    try {
      containers = (await get<{ containers: Container[] }>(`${base}/unmanaged`)).containers;
    } catch (e) {
      failure(e);
    } finally {
      loading = null;
    }
  }
  async function refresh() {
    await (tab === 'managed' ? loadManaged() : loadUnmanaged());
    notify.success('Resource statuses refreshed.');
  }
  async function act(action: 'start' | 'restart' | 'stop', id: string) {
    try {
      const r = await post<{ message: string }>(`${base}/unmanaged/${action}`, { id });
      notify.success(r.message);
      await loadUnmanaged();
    } catch (e) {
      failure(e);
    }
  }

  const q = $derived(search.trim().toLowerCase());
  const managedRows = $derived([...resources].sort((a, b) => strnatcmp(a.name ?? '', b.name ?? '')).filter((r) => !q || (r.name ?? '').toLowerCase().includes(q)));
  const unmanagedRows = $derived([...containers].sort((a, b) => strnatcmp(a.Names ?? '', b.Names ?? '')).filter((c) => !q || (c.Names ?? '').toLowerCase().includes(q)));
  const total = $derived(tab === 'managed' ? managedRows.length : unmanagedRows.length);
  $effect(() => {
    const last = Math.max(1, Math.ceil(total / pageSize));
    if (page > last) page = last;
  });
  const slice = <T,>(list: T[]) => list.slice((page - 1) * pageSize, page * pageSize);

  const statusType = (s: string): 'success' | 'error' | 'neutral' => (s.includes('running') ? 'success' : s.includes('failed') || s.includes('exited') ? 'error' : 'neutral');
  const tabClass = (on: boolean) =>
    `inline-flex items-center gap-1.5 rounded-md px-3 py-1 text-xs font-medium transition-colors disabled:cursor-wait ${
      on ? 'bg-white text-neutral-950 shadow-sm dark:bg-brand/15 dark:text-brand-light' : 'text-neutral-500 hover:text-neutral-900 dark:text-fg-dim dark:hover:text-fg'
    }`;
</script>

<ServerFrame {uuid} active="resources" title="Server Resources" onloaded={() => loadManaged()}>
  {#snippet children()}
    <div class="application-settings-form min-w-0 w-full">
      <SettingsSection id="server-resources-section" title="Resources" description="Review coolify-rs-managed resources and other Docker containers running on this server." flush>
        {#snippet actions()}
          <div class="inline-flex w-fit rounded-[10px] bg-neutral-100 p-1 dark:bg-white/[0.05]">
            <button type="button" class={tabClass(tab === 'managed')} disabled={loading !== null} onclick={loadManaged}>Managed</button>
            <button type="button" class={tabClass(tab === 'unmanaged')} disabled={loading !== null} onclick={loadUnmanaged}>Unmanaged</button>
          </div>
          <button type="button" class="button" onclick={refresh}>
            <Reicon name="refresh" class="size-3.5" />
            Refresh
          </button>
          <ViewToggle single canvasUrl="/server/{uuid}/canvas" />
        {/snippet}

        <div class="border-b border-neutral-200 p-3 dark:border-white/[0.08]">
          <div class="relative w-full max-w-sm">
            <Reicon name="search" class="pointer-events-none absolute top-1/2 left-2.5 z-10 size-3.5 -translate-y-1/2 text-neutral-400 dark:text-fg-faint" />
            <input
              bind:value={search}
              oninput={() => (page = 1)}
              type="search"
              placeholder="Search resources by name"
              aria-label="Search resources by name"
              class="h-8! w-full rounded-lg! border-neutral-200! bg-white! py-0! pr-8! pl-8! text-[12px]! shadow-none! placeholder:text-neutral-400 focus:border-accent! focus:ring-0! dark:border-white/[0.08]! dark:bg-white/[0.035]! dark:text-fg! dark:placeholder:text-fg-faint"
            />
            <button
              type="button"
              onclick={() => (search = '')}
              class="absolute top-1/2 right-2 flex size-5 -translate-y-1/2 items-center justify-center rounded text-neutral-400 transition-colors hover:bg-neutral-100 hover:text-black dark:text-fg-faint dark:hover:bg-white/[0.07] dark:hover:text-fg {search ? '' : 'hidden'}"
              aria-label="Clear search"
            >
              <Reicon name="x" class="size-3" />
            </button>
          </div>
        </div>

        <div class="relative">
          <div class="transition-all">
            {#if tab === 'managed'}
              {#if total > 0}
                <div class="data-table">
                  <div class="data-table-header server-resources-managed-table-grid">
                    <span>Name</span>
                    <span>Project</span>
                    <span>Environment</span>
                    <span>Type</span>
                    <span>Status</span>
                  </div>
                  {#each slice(managedRows) as r (r.type + r.uuid)}
                    <div class="data-table-row server-resources-managed-table-grid border-b border-neutral-200 last:border-b-0 dark:border-white/[0.08]">
                      <div class="flex min-w-0 items-center gap-2">
                        <span class="flex size-5 shrink-0 items-center justify-center overflow-hidden rounded text-neutral-500 dark:text-fg-dim">
                          <ResourceLogo databaseType={r.type === 'application' || r.type === 'service' ? '' : r.type} application={r.application ?? null} iconClass="size-3.5" />
                        </span>
                        <a class="block min-w-0 truncate text-[12px] font-medium text-neutral-950 hover:underline dark:text-fg" href={r.link} use:link>{r.name}</a>
                      </div>
                      <div class="truncate text-[11px] text-neutral-600 dark:text-fg-dim">{r.project}</div>
                      <div class="truncate text-[11px] text-neutral-600 dark:text-fg-dim">{r.environment}</div>
                      <div class="text-[11px] text-neutral-600 dark:text-fg-dim">{headline(r.type)}</div>
                      <div>
                        {#if r.restart_limit}
                          <span title="Container has crashed and coolify-rs stopped it after {r.restart_count} restart attempts."><StatusBadge label="Restart limit reached" type="warning" /></span>
                        {:else}
                          <StatusBadge label={headline(r.status ?? 'unknown')} type={statusType(r.status ?? 'unknown')} />
                        {/if}
                      </div>
                    </div>
                  {/each}
                </div>
              {:else}
                <div class="p-6">
                  <Empty size="sm" title={q ? 'No matching resources' : 'No managed resources'} description={q ? 'Try another name or clear the search.' : 'Resources assigned to this server will appear here.'} iconName="projects" />
                </div>
              {/if}
            {:else if total > 0}
              <div class="data-table">
                <div class="data-table-header server-resources-unmanaged-table-grid">
                  <span>Name</span>
                  <span>Image</span>
                  <span>Status</span>
                  <span>Actions</span>
                </div>
                {#each slice(unmanagedRows) as c (c.ID)}
                  <div class="data-table-row server-resources-unmanaged-table-grid border-b border-neutral-200 last:border-b-0 dark:border-white/[0.08]">
                    <div class="min-w-0 truncate text-[12px] font-medium text-neutral-950 dark:text-fg">{c.Names}</div>
                    <div class="min-w-0 truncate font-mono text-[11px] text-neutral-600 dark:text-fg-dim">{c.Image}</div>
                    <div>
                      <StatusBadge label={headline(c.State ?? 'unknown')} type={c.State === 'running' ? 'success' : c.State === 'exited' ? 'error' : 'warning'} />
                    </div>
                    <div class="flex flex-wrap items-center gap-2">
                      {#if c.State === 'running'}
                        <button type="button" class="button" disabled={!canEdit} onclick={() => act('restart', c.ID)}>Restart</button>
                        <button type="button" class="button button-error" disabled={!canEdit} onclick={() => act('stop', c.ID)}>Stop</button>
                      {:else if c.State === 'exited'}
                        <button type="button" class="button" disabled={!canEdit} onclick={() => act('start', c.ID)}>Start</button>
                      {:else if c.State === 'restarting'}
                        <button type="button" class="button" disabled={!canEdit} onclick={() => act('stop', c.ID)}>Stop</button>
                      {/if}
                    </div>
                  </div>
                {/each}
              </div>
            {:else}
              <div class="p-6">
                <Empty size="sm" title={q ? 'No matching containers' : 'No unmanaged containers'} description={q ? 'Try another name or clear the search.' : 'All detected Docker containers are managed by coolify-rs.'} iconName="servers" />
              </div>
            {/if}
            {#if total > 0}
              <Pager {total} bind:page bind:pageSize storageKey="coolify.page-size.server-resources" separator="–" />
            {/if}
          </div>
        </div>
      </SettingsSection>
    </div>
  {/snippet}
</ServerFrame>
