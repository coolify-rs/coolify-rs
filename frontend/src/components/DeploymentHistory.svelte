<script lang="ts">
  // Port of upstream livewire/project/application/deployment/index.blade.php (+
  // Deployment\Index): the "Deployment history" table with search, filters,
  // sort and paging, standalone on the Deployment Logs page or embedded beside
  // the logs of one deployment.
  import { untrack } from 'svelte';
  import Reicon from './Reicon.svelte';
  import Empty from './Empty.svelte';
  import Pager from './Pager.svelte';
  import Dropdown from './Dropdown.svelte';
  import StatusBadge from './StatusBadge.svelte';
  import Loading from './Loading.svelte';
  import SettingsSection from './SettingsSection.svelte';
  import SortHeader from './SortHeader.svelte';
  import { appUi, type DeploymentListItem, type DeploymentPage } from '../lib/api';
  import { link } from '../lib/router.svelte';
  import { diffForHumans, duration, serverTime } from '../lib/format';

  let {
    uuid,
    base,
    serverName,
    timezone,
    embedded = false,
    selected = null,
    onselect,
  }: {
    uuid: string;
    base: string;
    serverName: string;
    timezone: string;
    embedded?: boolean;
    selected?: string | null;
    onselect?: (dep: string) => Promise<void>;
  } = $props();

  let data = $state<DeploymentPage | null>(null);
  let search = $state('');
  let filters = $state<string[]>([]);
  // `protected $queryString = ['pull_request_id']`: an invalid value is dropped.
  function pullRequestFromUrl(): number | null {
    const raw = new URLSearchParams(location.search).get('pull_request_id');
    const n = raw === null || raw === '' ? NaN : Number(raw);
    return Number.isFinite(n) && n > 0 && n <= 2147483647 ? Math.trunc(n) : null;
  }
  let pullRequestId = $state<number | null>(pullRequestFromUrl());
  // Livewire writes the query string only when the property changes, not on opening the page.
  let urlOnOpen = pullRequestId;
  $effect(() => {
    if (embedded || pullRequestId === urlOnOpen) return;
    urlOnOpen = -1;
    const q = new URLSearchParams(location.search);
    if (pullRequestId) q.set('pull_request_id', String(pullRequestId));
    else q.delete('pull_request_id');
    const search = q.toString();
    history.replaceState(history.state, '', `${location.pathname}${search ? `?${search}` : ''}`);
  });
  let sort = $state<'newest' | 'oldest'>('newest');
  let page = $state(1);
  let pageSize = $state(untrack(() => (embedded ? 3 : 10)));
  let loading = $state(false);
  let filtering = $state(false);
  let sorting = $state(false);
  let searching = $state(false);
  let selecting = $state<string | null>(null);
  let tick = $state(0);

  const listUrl = $derived(`${base}/deployment`);
  const count = $derived(data?.count ?? 0);
  const activeFilterCount = $derived(filters.length + (pullRequestId ? 1 : 0));
  const hasActiveQuery = $derived(search.trim() !== '' || activeFilterCount > 0);

  let seq = 0;
  async function reload() {
    const mine = ++seq;
    const r = await appUi.deployments(uuid, {
      skip: (page - 1) * pageSize,
      take: pageSize,
      search: search.trim(),
      order: sort,
      filters,
      pullRequestId,
    });
    if (mine === seq) data = r;
  }

  async function run(flag: 'loading' | 'filtering', fn: () => void) {
    fn();
    if (flag === 'filtering') filtering = true;
    loading = true;
    try {
      await reload();
    } finally {
      loading = false;
      filtering = false;
    }
  }

  // Reload when the page or size changes.
  $effect(() => {
    void page;
    void pageSize;
    reload().catch(() => {});
  });

  // `setDeploymentSort()`: back to the first page; the Started header shows a spinner meanwhile.
  async function setSort(value: string) {
    if (value !== 'newest' && value !== 'oldest') return;
    sorting = true;
    try {
      await run('loading', () => {
        sort = value;
        page = 1;
      });
    } finally {
      sorting = false;
    }
  }

  // `wire:model.live.debounce.300ms="search"` resets to the first page.
  let searchTimer: ReturnType<typeof setTimeout> | undefined;
  function onsearch() {
    clearTimeout(searchTimer);
    searchTimer = setTimeout(async () => {
      searching = true;
      page = 1;
      try {
        await reload();
      } finally {
        searching = false;
      }
    }, 300);
  }

  // `wire:poll.5000ms="reloadDeployments"` only on the first page; also refreshes relative times.
  $effect(() => {
    const t = setInterval(() => {
      tick++;
      if (page === 1) reload().catch(() => {});
    }, 5000);
    return () => clearInterval(t);
  });

  function toggleFilter(value: string) {
    run('filtering', () => {
      filters = filters.includes(value) ? filters.filter((f) => f !== value) : [...filters, value];
      pullRequestId = null;
      page = 1;
    });
  }
  function setPullRequest(pr: number) {
    run('filtering', () => {
      pullRequestId = pr;
      filters = [];
      page = 1;
    });
  }
  function clearFilter() {
    run('filtering', () => {
      filters = [];
      pullRequestId = null;
      page = 1;
    });
  }

  function statusLabel(s: string): string {
    const map: Record<string, string> = { finished: 'Success', in_progress: 'In progress', 'cancelled-by-user': 'Cancelled', queued: 'Queued' };
    return map[s] ?? headline(s);
  }
  function statusType(s: string): 'success' | 'warning' | 'error' | 'neutral' {
    if (s === 'finished') return 'success';
    if (s === 'in_progress' || s === 'queued') return 'warning';
    if (s === 'failed') return 'error';
    return 'neutral';
  }
  function headline(s: string): string {
    return s
      .split(/[-_\s]+/)
      .filter(Boolean)
      .map((w) => w[0].toUpperCase() + w.slice(1))
      .join(' ');
  }
  function source(d: DeploymentListItem): string {
    const pr = d.pull_request_id ?? 0;
    if (d.is_webhook && pr > 0) return `Webhook · PR #${pr}`;
    if (d.is_webhook) return 'Webhook';
    if (pr > 0) return `Pull request #${pr}`;
    if (d.rollback) return 'Rollback';
    if (d.is_api) return 'API';
    return 'Manual';
  }
  // `$sourceKey` of the row: the `source:` filter value.
  function sourceKey(d: DeploymentListItem): string {
    if ((d.pull_request_id ?? 0) > 0) return 'pull-request';
    if (d.is_webhook) return 'webhook';
    if (d.rollback) return 'rollback';
    if (d.is_api) return 'api';
    return 'manual';
  }
  function durationOf(d: DeploymentListItem): string {
    void tick;
    if (d.status === 'queued') return 'Waiting';
    if (d.status === 'in_progress') return duration(d.created_at, 'now') ?? '-';
    return d.finished_at ? (duration(d.created_at, d.finished_at) ?? '-') : '-';
  }
  const ago = (d: DeploymentListItem, short = false) => (void tick, diffForHumans(d.created_at, short));

  async function rowClick(e: MouseEvent) {
    if (!embedded || !onselect) return;
    const a = (e.target as HTMLElement).closest('a[data-select-deployment]') as HTMLAnchorElement | null;
    if (!a || e.button !== 0 || e.metaKey || e.ctrlKey || e.shiftKey || e.altKey) return;
    e.preventDefault();
    e.stopPropagation();
    selecting = a.dataset.selectDeployment ?? null;
    try {
      if (selecting) await onselect(selecting);
    } finally {
      selecting = null;
    }
  }

  // Embedded rows select in place; standalone rows navigate.
  function rowLink(node: HTMLAnchorElement) {
    return embedded ? undefined : link(node);
  }
</script>

{#snippet check(selectedOption: boolean)}
  <span
    class={[
      'flex size-4 shrink-0 items-center justify-center rounded-[5px] border',
      selectedOption
        ? 'border-coollabs bg-coollabs text-white dark:border-brand dark:bg-brand dark:text-white'
        : 'border-neutral-300 bg-white dark:border-white/[0.14] dark:bg-white/[0.045]',
    ].join(' ')}
  >
    {#if selectedOption}
      <svg class="size-3" viewBox="0 0 12 12" fill="none" aria-hidden="true">
        <path d="m2.25 6.15 2.35 2.3 5.15-5" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" />
      </svg>
    {/if}
  </span>
{/snippet}

{#snippet tick14()}
  <svg class="size-3.5 shrink-0" viewBox="0 0 24 24" fill="none">
    <path d="m4.5 12.75 6 6 9-13.5" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round" />
  </svg>
{/snippet}

{#snippet group(title: string, options: { value: string; label: string }[], truncate = false)}
  {#if options.length > 0}
    <span class="px-2 pb-1 pt-2 text-[10px] font-medium uppercase tracking-wider text-neutral-400 dark:text-fg-faint">{title}</span>
    {#each options as option (option.value)}
      <button type="button" class="listbox-option" role="option" aria-selected={filters.includes(option.value)} onclick={() => toggleFilter(option.value)}>
        <span class={truncate ? 'truncate' : ''}>{option.label}</span>
        {@render check(filters.includes(option.value))}
      </button>
    {/each}
  {/if}
{/snippet}

<div class="application-settings-form min-w-0 {embedded ? 'deployment-history-fit' : ''}">
  <SettingsSection title="Deployment history" description="Search, filter, and open a deployment to inspect its build logs." flush>
    <div class="table-toolbar flex flex-col gap-2 sm:flex-row sm:flex-wrap sm:items-center border-b border-neutral-200 p-3 dark:border-white/[0.08]">
      <div class="min-w-0 w-full flex-1 sm:max-w-md">
        <div class="table-search relative min-w-0 w-full">
          <input type="search" placeholder="Search deployments" aria-label="Search deployments" class="input w-full pl-8!" bind:value={search} oninput={onsearch} />
          <div class="pointer-events-none absolute inset-y-0 left-0 flex items-center pl-2.5">
            {#if searching}
              <svg aria-hidden="true" class="size-3.5 animate-spin text-neutral-400 dark:text-fg-dim" fill="none" viewBox="0 0 24 24">
                <circle class="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" stroke-width="4" />
                <path
                  class="opacity-75"
                  fill="currentColor"
                  d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4zm2 5.291A7.962 7.962 0 014 12H0c0 3.042 1.135 5.824 3 7.938l3-2.647z"
                />
              </svg>
            {:else}
              <Reicon name="search" class="size-3.5 text-neutral-400 dark:text-fg-faint" />
            {/if}
          </div>
        </div>
      </div>
      <div class="flex flex-wrap items-center gap-2 sm:ml-auto">
        <div class="table-filter">
          <Dropdown panelClass="w-44! overflow-hidden! p-0!">
            {#snippet trigger({ open })}
              <button
                type="button"
                aria-haspopup="listbox"
                aria-expanded={open}
                title={pullRequestId ? `Pull request #${pullRequestId}` : undefined}
                class="button max-w-80 min-w-0 {activeFilterCount > 0 ? 'button-highlighted' : ''}"
              >
                <Reicon name="filter" class="size-3.5 shrink-0" />
                <span class="truncate">{pullRequestId ? `Pull request #${pullRequestId}` : 'Filter'}</span>
                {#if activeFilterCount > 0}
                  <span
                    class="shrink-0 rounded-full bg-neutral-100 px-1.5 py-0.5 text-[10px] font-medium text-neutral-500 dark:bg-white/[0.07] dark:text-fg-dim"
                    >{activeFilterCount}</span
                  >
                {/if}
              </button>
            {/snippet}
            {#snippet children({ close })}
              <div class="max-h-80 overflow-y-auto p-1">
                {@render group('Status', data?.status_options ?? [])}
                {@render group('Source', data?.source_options ?? [])}
                {@render group('Server', data?.server_options ?? [], true)}
                {#if (data?.pull_requests.length ?? 0) > 0}
                  <span class="px-2 pb-1 pt-2 text-[10px] font-medium uppercase tracking-wider text-neutral-400 dark:text-fg-faint">Pull request</span>
                  {#each data?.pull_requests ?? [] as pr (pr)}
                    <button
                      type="button"
                      class="listbox-option"
                      role="option"
                      aria-selected={pullRequestId === pr}
                      onclick={() => {
                        setPullRequest(pr);
                        close();
                      }}
                    >
                      <span>Pull request #{pr}</span>
                      {#if pullRequestId === pr}{@render tick14()}{/if}
                    </button>
                  {/each}
                {/if}
              </div>
              <div class="border-t border-neutral-200 bg-white p-1 dark:border-white/10 dark:bg-raised">
                <button
                  type="button"
                  class="listbox-option text-neutral-500 dark:text-fg-dim"
                  disabled={activeFilterCount === 0}
                  onclick={() => {
                    clearFilter();
                    close();
                  }}
                >
                  <span>Reset filters</span>
                  <Reicon name="x" class="size-3.5" />
                </button>
              </div>
            {/snippet}
          </Dropdown>
        </div>
        <!-- The Started header sorts the full table; the embedded history has no such header. -->
        {#if embedded}
        <div class="table-sort">
          <Dropdown panelClass="w-44!">
            {#snippet trigger({ open })}
              <button type="button" class="button" aria-haspopup="listbox" aria-expanded={open}>
                <Reicon name="sort-direction" class="size-3.5" />
                Sort
              </button>
            {/snippet}
            {#snippet children({ close })}
              {#each [['newest', 'Newest first'], ['oldest', 'Oldest first']] as [value, label] (value)}
                <button
                  type="button"
                  class="listbox-option"
                  role="option"
                  aria-selected={sort === value}
                  onclick={() => {
                    close();
                    void setSort(value);
                  }}
                >
                  <span>{label}</span>
                  {#if sort === value}{@render tick14()}{/if}
                </button>
              {/each}
            {/snippet}
          </Dropdown>
        </div>
        {/if}
      </div>
    </div>

    {#if data && data.deployments.length > 0}
      <div class="data-table relative w-full transition-opacity {loading ? 'opacity-50 pointer-events-none' : ''}">
        {#if filtering}
          <div
            class="table-loading-overlay absolute inset-0 z-30 flex items-center justify-center bg-white/70 backdrop-blur-[1px] dark:bg-black/20 rounded-lg"
          >
            <span class="sr-only">Filtering deployments...</span>
          </div>
        {/if}
        <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
        <div class="deployment-table-scroll {embedded ? 'deployment-table-embedded' : ''}" onclickcapture={rowClick}>
          <div class="data-table-header deployment-table-grid rounded-none!">
            {#if !embedded}<span>Status</span>{/if}
            <span>Source</span>
            <span>Commit</span>
            {#if !embedded}
              <SortHeader label="Started" asc="oldest" desc="newest" first="desc" current={sort} onsort={setSort} server loading={sorting} />
            {/if}
            <span>Duration</span>
            <span>Server</span>
            {#if embedded}<span class="text-right">Status</span>{/if}
          </div>

          {#each data.deployments as d (d.deployment_uuid)}
            {@const url = `${listUrl}/${d.deployment_uuid}`}
            {@const type = statusType(d.status)}
            {@const label = statusLabel(d.status)}
            {@const server = d.server_name || serverName || '-'}
            {@const commitMessage = d.commit_message ? d.commit_message.split('\n')[0] : null}
            <div
              class="data-table-row deployment-table-row deployment-table-grid group relative border-b border-neutral-200 text-[13px] text-neutral-600 hover:text-neutral-950 dark:border-white/[0.08] dark:text-fg-dim dark:hover:text-white"
              class:data-table-row-active={selected === d.deployment_uuid || selecting === d.deployment_uuid}
              aria-busy={embedded ? selecting === d.deployment_uuid : undefined}
            >
              {#if !embedded}
                <span class="min-w-0">
                  <StatusBadge
                    as="button"
                    {label}
                    {type}
                    spinning={d.status === 'in_progress'}
                    class="table-filter-value table-filter-badge"
                    title="Filter by {label}"
                    onclick={() => toggleFilter(`status:${d.status}`)}
                  />
                </span>
              {/if}
              {#if embedded}
                <a href={url} use:rowLink data-select-deployment={d.deployment_uuid}>{source(d)}</a>
              {:else}
                <span class="min-w-0">
                  <button type="button" class="table-filter-value truncate" title="Filter by {source(d)}" onclick={() => toggleFilter(`source:${sourceKey(d)}`)}
                    >{source(d)}</button
                  >
                </span>
              {/if}
              <span class="min-w-0">
                {#if d.commit}
                  <span class="flex min-w-0 items-center gap-2">
                    <span
                      class="shrink-0 font-mono text-xs text-neutral-950 underline decoration-neutral-400 underline-offset-2 dark:text-fg dark:decoration-neutral-600 group-hover:text-neutral-950 dark:group-hover:text-white"
                    >
                      {d.commit.slice(0, 7)}
                    </span>
                    {#if commitMessage}
                      <a
                        href={url}
                        use:rowLink
                        data-select-deployment={embedded ? d.deployment_uuid : undefined}
                        class="truncate text-neutral-500 dark:text-fg-faint group-hover:text-neutral-950 dark:group-hover:text-white"
                        title={commitMessage}>{commitMessage}</a
                      >
                    {/if}
                  </span>
                {:else}
                  <span class="text-neutral-400 dark:text-fg-faint group-hover:text-neutral-950 dark:group-hover:text-white">-</span>
                {/if}
                {#if embedded}
                  <span
                    class="mt-0.5 hidden truncate text-xs text-neutral-500 group-hover:text-neutral-950 dark:text-fg-faint dark:group-hover:text-white 2xl:block"
                  >
                    {source(d)} · <span class="tabular-nums">{durationOf(d)}</span> · {server}
                  </span>
                {/if}
              </span>
              {#if !embedded}
                <a href={url} use:rowLink title={serverTime(d.created_at, timezone)}>{ago(d)}</a>
              {/if}
              <a href={url} use:rowLink data-select-deployment={embedded ? d.deployment_uuid : undefined} class="tabular-nums">{durationOf(d)}</a>
              {#if !embedded && d.server_id}
                <span class="min-w-0">
                  <button type="button" class="table-filter-value truncate" title="Filter by {server}" onclick={() => toggleFilter(`server:${d.server_id}`)}
                    >{server}</button
                  >
                </span>
              {:else}
                <a href={url} use:rowLink data-select-deployment={embedded ? d.deployment_uuid : undefined} class="truncate">{server}</a>
              {/if}
              {#if embedded}
                <a
                  href={url}
                  data-select-deployment={d.deployment_uuid}
                  class="flex items-center justify-end gap-2 pr-1 tabular-nums"
                  title="{label} · {serverTime(d.created_at, timezone)}"
                >
                  <span>{ago(d, true)}</span>
                  {#if selecting === d.deployment_uuid}
                    <svg class="loading-indicator size-3.5 animate-spin" fill="none" viewBox="0 0 24 24" aria-label="Loading deployment logs...">
                      <circle class="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" stroke-width="4"></circle>
                      <path
                        class="opacity-75"
                        fill="currentColor"
                        d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4zm2 5.291A7.962 7.962 0 014 12H0c0 3.042 1.135 5.824 3 7.938l3-2.647z"
                      ></path>
                    </svg>
                  {:else if d.status === 'in_progress'}
                    <Loading compact label="Deployment in progress" />
                  {:else}
                    <span
                      class="size-2 shrink-0 rounded-full {{
                        success: 'bg-emerald-500',
                        warning: 'bg-warning motion-safe:animate-pulse',
                        error: 'bg-red-500',
                        neutral: 'bg-neutral-400 dark:bg-neutral-500',
                      }[type]}"
                    ></span>
                  {/if}
                </a>
              {/if}
              <a
                href={url}
                use:rowLink
                data-select-deployment={embedded ? d.deployment_uuid : undefined}
                class="deployment-row-link absolute inset-0"
                aria-hidden="true"
                tabindex="-1"
              ></a>
            </div>
          {/each}
        </div>

        <Pager
          total={count}
          bind:page
          bind:pageSize
          separator="–"
          sizeSelect={!embedded}
          storageKey={embedded ? undefined : 'coolify.page-size.deployments'}
        />
      </div>
    {:else if data}
      <Empty
        size="sm"
        title="No deployments found"
        description={hasActiveQuery ? 'No deployments match the current search and filters.' : 'Deploy the application to create its first deployment record.'}
        iconName="layers"
      />
    {/if}
  </SettingsSection>
</div>
