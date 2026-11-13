<script lang="ts">
  // Port of upstream livewire/image-scan/fleet-vulnerabilities.blade.php
  // (ImageScan\FleetVulnerabilities): the team Security page (spec 15.8): the resources the user
  // can see with their stored security status, most urgent first, and a search by vulnerability
  // ID or package. coolify-rs has no push channel, so the page reloads its results every
  // RELOAD_INTERVAL seconds instead of on scan events.
  import { untrack } from 'svelte';
  import Callout from '../components/Callout.svelte';
  import Empty from '../components/Empty.svelte';
  import Pager from '../components/Pager.svelte';
  import ResourceLogo from '../components/ResourceLogo.svelte';
  import StatusBadge from '../components/StatusBadge.svelte';
  import CheckMark from '../components/imageScan/CheckMark.svelte';
  import ExportMenu from '../components/imageScan/ExportMenu.svelte';
  import SeverityBadge from '../components/imageScan/SeverityBadge.svelte';
  import TableFilter from '../components/imageScan/TableFilter.svelte';
  import TableSearch from '../components/imageScan/TableSearch.svelte';
  import { request, ApiError } from '../lib/api';
  import { link, router } from '../lib/router.svelte';
  import { notify } from '../lib/toast.svelte';

  const RELOAD_INTERVAL = 10;

  interface Entry {
    key: string;
    name: string;
    link: string | null;
    logo: {
      application?: { buildPack: string | null; sourceType: string | null; fqdn: string | null; basicAuth: boolean };
      service?: { logo: { src: string; fallback: string } | null };
      databaseType?: string;
    };
    project_name: string | null;
    environment_name: string | null;
    server_name: string | null;
    status: string;
    status_label: string;
    badge_type: 'error' | 'warning' | 'neutral' | null;
    images_line: string | null;
    action: { text: string; link: boolean; muted: boolean } | null;
    checked_at: { human: string; title: string } | null;
    match: {
      id: string;
      package: string;
      version: string;
      fixed_in: string | null;
      severity: string;
      state: string;
      state_label: string;
      more: number;
      more_text: string;
    } | null;
  }

  interface FleetData {
    team_id: number;
    items: Entry[];
    pagination: { total: number; page: number; per_page: number; last_page: number };
    shown_count: number;
    searching: boolean;
    not_scanned_line: string | null;
    not_checked_toggle: string | null;
    hides_not_checked: boolean;
    search_capped: boolean;
    max_decoded_scans: number;
    servers_not_scanning: number;
    server_options: { value: string; label: string }[];
    project_options: { value: string; label: string }[];
    status_options: { value: string; label: string }[];
    has_filters: boolean;
    has_resources: boolean;
    servers: string[];
    projects: string[];
    statuses: string[];
  }

  document.title = 'Security | coolify-rs';

  const groupLabel = 'px-2 pb-1 pt-2 text-[10px] font-medium uppercase tracking-wider text-neutral-400 dark:text-fg-faint';
  const muted = 'text-[11px] text-neutral-500 dark:text-fg-faint';
  const secondary = 'truncate text-[11px] text-neutral-600 dark:text-fg-dim';

  /** `#[Url]` properties: `?search=`, `?statuses[0]=`, `?showNotChecked=1`. */
  function fromUrl() {
    const q = new URLSearchParams(router.search);
    const statuses: string[] = [];
    for (const [k, v] of q) if (/^statuses\[\d*\]$/.test(k)) statuses.push(v);
    return { search: q.get('search') ?? '', statuses, showNotChecked: q.get('showNotChecked') === '1' || q.get('showNotChecked') === 'true' };
  }
  const initial = untrack(fromUrl);

  let search = $state(initial.search);
  let servers = $state<string[]>([]);
  let projects = $state<string[]>([]);
  let statuses = $state<string[]>(initial.statuses);
  let showNotChecked = $state(initial.showNotChecked);
  let page = $state(1);
  let perPage = $state(25);
  let data = $state<FleetData | null>(null);
  let loading = $state(false);
  let searching = $state(false);
  let request_id = 0;

  function currentState() {
    return { search, servers, projects, statuses, show_not_checked: showNotChecked, page, per_page: perPage };
  }

  function writeUrl() {
    const q = new URLSearchParams();
    if (search !== '') q.set('search', search);
    statuses.forEach((s, i) => q.set(`statuses[${i}]`, s));
    if (showNotChecked) q.set('showNotChecked', '1');
    const qs = q.toString();
    history.replaceState(history.state, '', `/vulnerabilities${qs ? `?${qs}` : ''}`);
  }

  async function load(quiet = false) {
    const id = ++request_id;
    if (!quiet) loading = true;
    try {
      const r = await request<FleetData>('POST', '/ui/vulnerabilities', currentState());
      if (id !== request_id) return;
      data = r;
      page = r.pagination.page;
      servers = r.servers;
      projects = r.projects;
      statuses = r.statuses;
    } catch (e) {
      if (!quiet) notify.failure(e instanceof ApiError ? e.message : String(e));
    } finally {
      if (id === request_id) {
        loading = false;
        searching = false;
      }
    }
  }

  function apply() {
    writeUrl();
    void load();
  }

  $effect(() => {
    untrack(() => void load());
    // A scan result came in somewhere in the team: render again, at most once per interval.
    const timer = setInterval(() => void load(true), RELOAD_INTERVAL * 1000);
    return () => clearInterval(timer);
  });

  function toggle(list: string[], value: string): string[] {
    return list.includes(value) ? list.filter((v) => v !== value) : [...list, value];
  }

  const serversKey = $derived(data ? `coolify.image-scan.servers-not-scanning-dismissed.${data.team_id}.${data.servers_not_scanning}` : '');
  let dismissed = $state<Record<string, boolean>>({});
  function isDismissed(key: string): boolean {
    if (dismissed[key] !== undefined) return dismissed[key];
    try {
      return localStorage.getItem(key) === '1';
    } catch {
      return false;
    }
  }
  function dismiss(key: string) {
    try {
      localStorage.setItem(key, '1');
    } catch {
      // Storage unavailable: hidden for this page only.
    }
    dismissed[key] = true;
  }

  const exportLoad = (format: string) => request<{ content: string; name?: string; mime?: string }>('POST', '/ui/vulnerabilities/export', { ...currentState(), format });
</script>

<div class="flex w-full min-w-0 flex-col gap-6">
  <div class="min-w-0">
    <h1 class="min-w-0 text-[24px]! leading-7! font-semibold! tracking-tight!">Security</h1>
    <p class="mt-1 text-[13px] text-neutral-500 dark:text-fg-dim">What each resource of your team needs, from its last image scan.</p>
  </div>

  {#if data && data.servers_not_scanning > 0 && !isDismissed(serversKey)}
    <div>
      <Callout
        type="info"
        title={data.servers_not_scanning === 1 ? '1 server does not scan images.' : `${data.servers_not_scanning} servers do not scan images.`}
        ondismiss={() => dismiss(serversKey)}
      >
        <a href="/servers" use:link class="font-medium text-coollabs hover:underline dark:text-brand-light">Go to servers</a>
      </Callout>
    </div>
  {/if}

  <div class="min-w-0 overflow-hidden rounded-xl border border-neutral-200 bg-white shadow-sm dark:border-white/[0.08] dark:bg-white/[0.05]">
    <div class="table-toolbar flex flex-col gap-2 sm:flex-row sm:flex-wrap sm:items-center border-b border-neutral-200 p-3 dark:border-white/[0.08]">
      <div class="min-w-0 w-full flex-1 sm:max-w-md">
        <TableSearch
          placeholder="Search by CVE, GHSA, or package"
          bind:value={search}
          loading={searching}
          onsearch={() => {
            searching = true;
            page = 1;
            apply();
          }}
        />
      </div>
      <div class="flex flex-wrap items-center gap-2 sm:ml-auto">
        <TableFilter
          activeCount={statuses.length + servers.length + projects.length}
          onreset={() => {
            servers = [];
            projects = [];
            statuses = [];
            page = 1;
            apply();
          }}
        >
          <span class={groupLabel}>Status</span>
          {#each data?.status_options ?? [] as option (option.value)}
            <button
              type="button"
              class="listbox-option"
              role="option"
              aria-selected={statuses.includes(option.value) ? 'true' : 'false'}
              onclick={() => {
                statuses = toggle(statuses, option.value);
                page = 1;
                apply();
              }}
            >
              <span>{option.label}</span>
              <CheckMark selected={statuses.includes(option.value)} />
            </button>
          {/each}
          {#if (data?.server_options.length ?? 0) > 0}
            <span class={groupLabel}>Server</span>
            {#each data?.server_options ?? [] as option (option.value)}
              <button
                type="button"
                class="listbox-option"
                role="option"
                aria-selected={servers.includes(option.value) ? 'true' : 'false'}
                onclick={() => {
                  servers = toggle(servers, option.value);
                  page = 1;
                  apply();
                }}
              >
                <span class="truncate" title={option.label}>{option.label}</span>
                <CheckMark selected={servers.includes(option.value)} />
              </button>
            {/each}
          {/if}
          {#if (data?.project_options.length ?? 0) > 0}
            <span class={groupLabel}>Project</span>
            {#each data?.project_options ?? [] as option (option.value)}
              <button
                type="button"
                class="listbox-option"
                role="option"
                aria-selected={projects.includes(option.value) ? 'true' : 'false'}
                onclick={() => {
                  projects = toggle(projects, option.value);
                  page = 1;
                  apply();
                }}
              >
                <span class="truncate" title={option.label}>{option.label}</span>
                <CheckMark selected={projects.includes(option.value)} />
              </button>
            {/each}
          {/if}
        </TableFilter>

        <ExportMenu count={data?.shown_count ?? 0} noun="resource" withIds={false} load={exportLoad} />
      </div>
    </div>

    <div class="relative">
      {#if loading}
        <div class="table-loading-overlay absolute inset-0 z-30 flex items-center justify-center bg-white/70 backdrop-blur-[1px] dark:bg-black/20">
          <span class="sr-only">Loading records...</span>
        </div>
      {/if}

      {#if data}
        {#if data.items.length === 0}
          {#if data.searching}
            <Empty size="sm" title="No scanned image has this vulnerability or package." description="Check the ID, or search for the package name." />
          {:else if data.has_filters}
            <Empty size="sm" title="No matching resources" description="Try other filters, or reset them." />
          {:else if !data.has_resources}
            <Empty size="sm" title="No resources yet" description="The resources of your team show here." />
          {:else if data.hides_not_checked}
            <Empty
              size="sm"
              title="No resource was scanned yet"
              description="Resources that were not scanned are hidden. Coolify scans an image when it deploys or starts it on a server that scans images."
            />
          {/if}
        {:else}
          <div class="data-table">
            <div class="data-table-header image-scan-fleet-grid">
              <span>Resource</span>
              <span>{data.searching ? 'Match' : 'Status'}</span>
              <span class="image-scan-fleet-action">{data.searching ? 'State' : 'Action'}</span>
              <span class="image-scan-secondary">Server</span>
              <span class="image-scan-secondary image-scan-fleet-tertiary">Checked</span>
            </div>
            {#each data.items as entry (entry.key)}
              <div class="data-table-row image-scan-fleet-grid border-b border-neutral-200 last:border-b-0 dark:border-white/[0.08]">
                <div class="flex min-w-0 items-center gap-2">
                  <span class="flex size-5 shrink-0 items-center justify-center overflow-hidden rounded text-neutral-500 dark:text-fg-dim">
                    <ResourceLogo
                      iconClass="size-3.5"
                      application={entry.logo.application ?? null}
                      service={entry.logo.service ?? null}
                      databaseType={entry.logo.databaseType ?? ''}
                    />
                  </span>
                  <div class="min-w-0">
                    <div class="flex min-w-0 items-center gap-1.5">
                      {#if entry.link}
                        <a href={entry.link} use:link class="block min-w-0 truncate text-[12px] font-medium text-neutral-950 hover:underline dark:text-fg" title={entry.name}
                          >{entry.name}</a
                        >
                      {:else}
                        <span class="block min-w-0 truncate text-[12px] font-medium text-neutral-950 dark:text-fg">{entry.name}</span>
                      {/if}
                      {#if data.searching && entry.badge_type !== null}
                        <StatusBadge type={entry.badge_type} label={entry.status_label} class="h-5! px-1.5! text-[10px]!" />
                      {/if}
                    </div>
                    <span class="block truncate {muted}" title="{entry.project_name ?? ''} / {entry.environment_name ?? ''}"
                      >{entry.project_name ?? ''} / {entry.environment_name ?? ''}</span
                    >
                  </div>
                </div>

                {#if data.searching && entry.match}
                  {@const match = entry.match}
                  <div class="flex min-w-0 flex-wrap items-center gap-1.5">
                    <SeverityBadge severity={match.severity} />
                    <span class="min-w-0 truncate font-mono text-[11px] text-neutral-700 dark:text-fg" title="{match.id} {match.package} {match.version}"
                      >{match.id} · {match.package} {match.version}</span
                    >
                    {#if match.more > 0}
                      <span class={muted}>+{match.more_text} more</span>
                    {/if}
                  </div>
                  <span class="image-scan-fleet-action text-[12px] text-neutral-700 dark:text-fg">
                    {match.state_label}{#if match.state === 'fixed' && match.fixed_in}<span class={muted}>{` → ${match.fixed_in}`}</span>{/if}
                  </span>
                {:else}
                  <div class="flex min-w-0 flex-col items-start justify-center gap-0.5">
                    {#if entry.badge_type !== null}
                      <StatusBadge type={entry.badge_type} label={entry.status_label} />
                    {:else}
                      <span class={muted}>{entry.status_label}</span>
                    {/if}
                    {#if entry.images_line !== null}
                      <span class="{muted} tabular-nums">{entry.images_line}</span>
                    {/if}
                  </div>
                  <div class="image-scan-fleet-action min-w-0">
                    {#if entry.action}
                      {@const actionClass = entry.action.muted ? 'text-[12px] text-neutral-500 dark:text-fg-faint' : 'text-[12px] text-neutral-800 dark:text-fg'}
                      {#if entry.action.link && entry.link}
                        <a
                          href="{entry.link}#security-section"
                          use:link
                          class="line-clamp-2 underline decoration-neutral-400/40 underline-offset-4 transition-colors hover:text-neutral-950 hover:decoration-current dark:decoration-white/25 dark:hover:text-fg dark:hover:decoration-current {actionClass}"
                          title={entry.action.text}>{entry.action.text}</a
                        >
                      {:else}
                        <span class="line-clamp-2 {actionClass}">{entry.action.text}</span>
                      {/if}
                    {/if}
                  </div>
                {/if}

                <span class="image-scan-secondary {secondary}" title={entry.server_name ?? ''}>{entry.server_name ?? ''}</span>
                <span class="image-scan-secondary image-scan-fleet-tertiary {muted}" title={entry.checked_at?.title}>{entry.checked_at?.human ?? ''}</span>
              </div>
            {/each}
          </div>
        {/if}
      {/if}
    </div>

    <Pager
      total={data?.pagination.total ?? 0}
      separator="–"
      storageKey="coolify.page-size.image-scan-fleet"
      shownSize={perPage}
      bind:page={() => page, (p) => p !== page && ((page = p), void load())}
      bind:pageSize={() => perPage, (s) => s !== perPage && ((perPage = s), (page = 1), void load())}
    />

    {#if data?.search_capped}
      <div class="border-t border-neutral-200 px-4 py-2.5 text-[12px] text-neutral-500 dark:border-white/[0.08] dark:text-fg-faint">
        Only the first {data.max_decoded_scans} images with a match were searched. Filter by server or project to see the rest.
      </div>
    {/if}
    {#if data?.not_scanned_line}
      <div class="border-t border-neutral-200 px-4 py-2.5 text-[12px] text-neutral-500 dark:border-white/[0.08] dark:text-fg-faint">
        {data.not_scanned_line}
        {#if data.not_checked_toggle !== null}
          <button
            type="button"
            onclick={() => {
              showNotChecked = !showNotChecked;
              page = 1;
              apply();
            }}
            class="font-medium text-neutral-700 underline decoration-neutral-400/40 underline-offset-4 transition-colors hover:text-neutral-950 hover:decoration-current dark:text-fg-dim dark:decoration-white/25 dark:hover:text-fg"
            >{data.not_checked_toggle}</button
          >
        {/if}
      </div>
    {/if}
  </div>
</div>
