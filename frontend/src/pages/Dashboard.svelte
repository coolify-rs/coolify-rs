<script lang="ts">
  // Port of upstream livewire/dashboard.blade.php.
  import Reicon from '../components/Reicon.svelte';
  import SectionHeading from '../components/SectionHeading.svelte';
  import ActiveDeployments from '../components/ActiveDeployments.svelte';
  import ServerMetricsChart from '../components/dashboard/ServerMetricsChart.svelte';
  import TrafficAnalytics from '../components/dashboard/TrafficAnalytics.svelte';
  import ViewToggle from '../components/ViewToggle.svelte';
  import Empty from '../components/Empty.svelte';
  import NewProjectModal from '../components/NewProjectModal.svelte';
  import PluginPanels from '../components/PluginPanels.svelte';
  import ResourceLogo from '../components/ResourceLogo.svelte';
  import Callout from '../components/Callout.svelte';
  import { session } from '../lib/session.svelte';
  import { dismissSecurityCallout, securityCallout, securityCalloutTitle } from '../lib/securityCallout';
  import { tick } from 'svelte';
  import { starsUi, type StarredResource } from '../lib/starsApi.svelte';
  import { uiApi, type DashboardData } from '../lib/api';
  import { link } from '../lib/router.svelte';
  import { listView } from '../lib/listview.svelte';
  import { strnatcasecmp } from '../lib/natsort';
  import SortHeader from '../components/SortHeader.svelte';
  import { serverMetricsPoll, type Point } from '../lib/metricsApi.svelte';

  const limit = 8;
  let data = $state<DashboardData | null>(null);
  let creating = $state(false);
  const projectsView = listView('dashboard_projects');
  const serversView = listView('dashboard_servers');

  const metrics = serverMetricsPoll();

  // `Dashboard::$projectSort`, `$serverSort` (`column-direction`).
  let projectSort = $state('name-asc');
  let serverSort = $state('name-asc');

  /** `Dashboard::sortItems()`: by the column, natural and case-insensitive, with the name as tie-breaker. */
  function sortItems<T extends { name: string }>(items: T[], sort: string, value: (item: T, column: string) => string | number): T[] {
    const [column, direction] = sort.split('-');
    const order = direction === 'desc' ? -1 : 1;
    return [...items]
      .sort((a, b) => strnatcasecmp(a.name, b.name))
      .sort((a, b) => order * strnatcasecmp(String(value(a, column)), String(value(b, column))));
  }
  const isValidSort = (sort: string, columns: string[]) => {
    const [column, direction] = sort.split('-');
    return columns.includes(column) && ['asc', 'desc'].includes(direction);
  };
  const sortedProjects = (d: DashboardData, sort: string) =>
    sortItems(d.projects, sort, (p, column) => (column === 'resources' ? p.resources : column === 'description' ? (p.description ?? '') : p.name));
  const sortedServers = (d: DashboardData, sort: string) =>
    sortItems(d.servers, sort, (s, column) => (column === 'description' ? (s.description ?? '') : column === 'status' ? s.status : s.name));
  const startMetrics = (d: DashboardData) =>
    metrics.start(sortedServers(d, serverSort).slice(0, limit).filter((s) => s.is_metrics_enabled).map((s) => s.uuid));
  function setProjectSort(sort: string) {
    if (isValidSort(sort, ['name', 'resources', 'description'])) projectSort = sort;
  }
  function setServerSort(sort: string) {
    if (!isValidSort(sort, ['name', 'description', 'status'])) return;
    serverSort = sort;
    if (data) startMetrics(data);
  }

  uiApi.dashboard().then((d) => {
    data = d;
    starred = d.starred_resources ?? [];
    startMetrics(d);
    showSecurityCallout(d.security_update_now ?? []);
  });

  // The callout "{n} resources need a security update now." (resources/js/security-callout.js):
  // dismissed until a resource that was not dismissed needs an update now.
  let securityVisible = $state(false);
  const securityKey = $derived(`coolify.image-scan.dashboard-update-now-dismissed.${session.info?.current_team?.id ?? ''}`);
  function readStorage(key: string): string | null {
    try {
      return localStorage.getItem(key);
    } catch {
      return null;
    }
  }
  function showSecurityCallout(resources: string[]) {
    const state = securityCallout(resources, readStorage(securityKey));
    securityVisible = state.visible;
    if (state.reset) {
      try {
        localStorage.removeItem(securityKey);
      } catch {
        // Storage unavailable.
      }
    }
  }
  function dismissSecurity() {
    try {
      localStorage.setItem(securityKey, dismissSecurityCallout(data?.security_update_now ?? [], readStorage(securityKey)));
    } catch {
      // Storage unavailable: hidden for this page only.
    }
    securityVisible = false;
  }

  // `$starredResources`: the cards of the "Starred" section, in the order the user chose.
  let starred = $state<StarredResource[]>([]);
  let starGrid: HTMLDivElement | undefined = $state();
  // The card being dragged (`dragged`), by star id, and the classes it wears meanwhile (`lifted`).
  let dragged = $state<number | null>(null);
  let sliding = false;
  const lifted = 'z-20 scale-[1.02] shadow-lg ring-2 ring-coollabs/40 dark:ring-brand-light/30 cursor-grabbing';

  /** `unstar()`. */
  async function unstar(starId: number) {
    await starsUi.unstar(starId);
    starred = starred.filter((s) => s.star_id !== starId);
  }
  // Pointer events, so the same code works for mouse, touch, and pen.
  function startDrag(event: PointerEvent, starId: number) {
    event.preventDefault();
    dragged = starId;
  }
  async function moveDrag(event: PointerEvent) {
    if (dragged === null || !starGrid || sliding) return;
    const grid = starGrid;
    const draggedCard = grid.querySelector<HTMLElement>(`[data-star-id="${dragged}"]`);
    const target = document
      .elementsFromPoint(event.clientX, event.clientY)
      .map((element) => element.closest<HTMLElement>('[data-star-id]'))
      .find((card) => card && card !== draggedCard && grid.contains(card));
    // A card that still slides would move away under the pointer and swap back.
    if (!target || target.getAnimations().length) return;
    const rect = target.getBoundingClientRect();
    const after = (event.clientX - rect.left) / rect.width + (event.clientY - rect.top) / rect.height > 1;
    const cards = [...grid.querySelectorAll<HTMLElement>('[data-star-id]')];
    const oldRects = new Map(cards.map((card) => [card, card.getBoundingClientRect()]));
    // `insertBefore(dragged, after ? target.nextSibling : target)` on the list the cards render from.
    const moved = starred.find((s) => s.star_id === dragged);
    if (!moved) return;
    const rest = starred.filter((s) => s.star_id !== dragged);
    const index = rest.findIndex((s) => s.star_id === Number(target.dataset.starId));
    if (index < 0) return;
    rest.splice(index + (after ? 1 : 0), 0, moved);
    starred = rest;
    sliding = true;
    await tick();
    sliding = false;
    if (window.matchMedia('(prefers-reduced-motion: reduce)').matches) return;
    // FLIP: each card that moved slides from its old place to the new one.
    cards.forEach((card) => {
      const oldRect = oldRects.get(card);
      if (!oldRect) return;
      const newRect = card.getBoundingClientRect();
      const dx = oldRect.left - newRect.left;
      const dy = oldRect.top - newRect.top;
      if (dx || dy) {
        card.animate([{ transform: `translate(${dx}px, ${dy}px)` }, { transform: 'none' }], { duration: 200, easing: 'cubic-bezier(0.2, 0, 0, 1)' });
      }
    });
  }
  /** Drop: `reorderStars()` with the star ids in their new order. */
  function endDrag() {
    if (dragged === null) return;
    dragged = null;
    starsUi.reorder(starred.map((s) => s.star_id)).catch(() => {});
  }

  const projects = $derived(data ? sortedProjects(data, projectSort).slice(0, limit) : []);
  const servers = $derived(data ? sortedServers(data, serverSort).slice(0, limit) : []);
  const navigateTo = (p: DashboardData['projects'][number]) =>
    p.environments.length === 1 ? `/project/${p.uuid}/environment/${p.environments[0].uuid}` : `/project/${p.uuid}`;
  const plural = (n: number, one: string, many: string) => (n === 1 ? one : many);

  const iconButton =
    'card-icon-button flex size-6.5 items-center justify-center rounded-md text-neutral-400 transition-colors hover:bg-neutral-100 hover:text-black';
  const titleIconButton =
    'flex size-5 items-center justify-center rounded-md text-neutral-400 transition-colors hover:bg-neutral-100 hover:text-black dark:text-fg-faint dark:hover:bg-white/[0.06] dark:hover:text-fg';
  const tableShell = 'overflow-x-auto rounded-xl border border-neutral-200 bg-white shadow-sm dark:border-white/[0.08] dark:bg-white/[0.05]';
  const tableHead =
    'border-b border-neutral-200 bg-neutral-50 px-4 py-2.5 text-[11px] font-medium text-neutral-500 dark:border-white/[0.08] dark:bg-white/[0.05] dark:text-fg-faint';
  const tableRow =
    'min-h-14 items-center border-b border-neutral-200 px-4 py-2.5 text-[12px] transition-colors last:border-b-0 hover:bg-neutral-50 dark:border-white/[0.07] dark:hover:bg-white/[0.025]';
  const card =
    'group relative flex min-h-28 min-w-0 flex-col rounded-xl border border-neutral-200 bg-white p-3 shadow-sm transition-all hover:-translate-y-px hover:border-neutral-300 hover:shadow-md dark:border-white/[0.08] dark:bg-white/[0.05] dark:hover:border-white/[0.14]';
  const glyph = 'resource-glyph flex size-8 shrink-0 items-center justify-center rounded-lg border border-neutral-200 bg-neutral-50 text-neutral-500';
  const projectCols = 'grid-cols-[minmax(0,1fr)_7rem_4.5rem] gap-4 @2xl:min-w-[560px] @2xl:grid-cols-[minmax(0,1.2fr)_7rem_minmax(0,1.5fr)_4.5rem]';
  // A Usage column (latest CPU and memory) only when a listed server collects metrics.
  const showServerUsage = $derived(servers.some((s) => s.is_metrics_enabled));
  const hasTrafficAnalytics = $derived(!!data?.servers.some((s) => s.is_traffic_analytics_enabled));
  const serverCols = $derived(
    showServerUsage
      ? 'grid-cols-[minmax(0,1fr)_9.5rem] @xl:grid-cols-[minmax(0,1fr)_9rem_9.5rem] @2xl:grid-cols-[minmax(0,1fr)_minmax(0,1.5fr)_9rem_9.5rem]'
      : 'grid-cols-[minmax(0,1fr)_9.5rem] @2xl:grid-cols-[minmax(0,1fr)_minmax(0,1.5fr)_9.5rem]',
  );
  const latest = (points: Point[] | null | undefined) => (points?.length ? points[points.length - 1][1] : null);
</script>

{#snippet projectActions(p: DashboardData['projects'][number])}
  {#if p.environments[0] && data?.can_create}
    <a
      href="/project/{p.uuid}/environment/{p.environments[0].uuid}/new"
      use:link
      class={iconButton}
      title="Add resource"
      aria-label="Add resource to {p.name}"
    >
      <Reicon name="plus" class="size-3" />
    </a>
  {/if}
  {#if data?.can_create}
    <a href="/project/{p.uuid}/edit" use:link class={iconButton} title="Project settings" aria-label="Open settings for {p.name}">
      <Reicon name="settings" class="size-3" />
    </a>
  {/if}
{/snippet}

<svelte:window onpointermove={moveDrag} onpointerup={endDrag} onpointercancel={endDrag} />

<div class="application-settings-form w-full">
  <div class="flex min-w-0 flex-col gap-8">
    {#if securityVisible && (data?.security_update_now?.length ?? 0) > 0}
      <div>
        <Callout type="danger" title={securityCalloutTitle(data?.security_update_now?.length ?? 0)} ondismiss={dismissSecurity}>
          <a href="/vulnerabilities?statuses[0]=update_now" use:link class="font-medium underline-offset-2 hover:underline">Review</a>
        </Callout>
      </div>
    {/if}

    <!-- First on every screen size, so starred resources are one tap away on phones too. -->
    {#if starred.length > 0}
      <section class="@container mb-0! min-w-0">
        <SectionHeading title="Starred" />
        <div bind:this={starGrid} class="grid min-w-0 grid-cols-1 gap-3 @lg:grid-cols-2 @5xl:grid-cols-3 @7xl:grid-cols-4">
          {#each starred as s (s.star_id)}
            <article
              data-star-id={s.star_id}
              class="group relative flex min-h-16 min-w-0 select-none items-center gap-3 rounded-xl [-webkit-touch-callout:none] border border-neutral-200 bg-white p-3 shadow-sm transition-all hover:-translate-y-px hover:border-neutral-300 hover:shadow-md dark:border-white/[0.08] dark:bg-white/[0.05] dark:hover:border-white/[0.14] {dragged === s.star_id ? lifted : ''}"
            >
              <a href={s.href} use:link class="absolute inset-0 rounded-xl" aria-label="Open {s.name}"></a>
              <!-- The handle has touch-none, so a drag on a phone does not scroll the page. -->
              <button
                type="button"
                title="Drag to change the order"
                aria-label="Move {s.name}"
                onpointerdown={(e) => startDrag(e, s.star_id)}
                class="relative z-10 -mx-1.5 flex h-8 w-5 shrink-0 cursor-grab touch-none items-center justify-center rounded-md text-neutral-400 transition-colors hover:bg-neutral-100 hover:text-black active:cursor-grabbing dark:text-fg-faint dark:hover:bg-white/[0.06] dark:hover:text-fg"
              >
                <Reicon name="grip-vertical" class="size-4" />
              </button>
              <div class="resource-glyph flex size-8 shrink-0 items-center justify-center overflow-hidden rounded-lg border border-neutral-200 bg-neutral-50 text-neutral-500">
                {#if s.type === 'server'}
                  <Reicon name="servers" class="size-4" />
                {:else}
                  <ResourceLogo
                    databaseType={s.database_type ?? ''}
                    application={s.application
                      ? {
                          buildPack: s.application.build_pack,
                          sourceType: s.application.source_type,
                          fqdn: s.application.fqdn,
                          basicAuth: s.application.is_http_basic_auth_enabled,
                        }
                      : null}
                    service={s.service}
                  />
                {/if}
              </div>
              <div class="min-w-0 flex-1">
                <h3 class="truncate text-[13px]! leading-4! font-semibold! text-black dark:text-fg">{s.name}</h3>
                <p class="mt-0.5 truncate text-[11px] text-neutral-500 dark:text-fg-faint">{s.context}</p>
              </div>
              <button
                type="button"
                onclick={() => unstar(s.star_id)}
                title="Remove from the dashboard"
                aria-label="Unstar {s.name}"
                class="relative z-10 flex size-6.5 shrink-0 items-center justify-center rounded-md text-warning transition-colors hover:bg-neutral-100 dark:hover:bg-white/[0.06]"
              >
                <Reicon name="star-filled" class="size-4" />
              </button>
            </article>
          {/each}
        </div>
      </section>
    {/if}

    <div class="grid min-w-0 gap-8 xl:items-start xl:has-[.dashboard-activity_section]:grid-cols-[minmax(0,1fr)_22rem] 2xl:has-[.dashboard-activity_section]:grid-cols-[minmax(0,1fr)_26rem]">
      <div class="dashboard-activity hidden min-w-0 has-[section]:flex flex-col gap-8 xl:order-last">
        <ActiveDeployments />

        {#if hasTrafficAnalytics}
          <TrafficAnalytics />
        {/if}
      </div>
      <div class="flex min-w-0 flex-col gap-8">
        <section class="@container mb-0! min-w-0">
          <SectionHeading title="Projects" href={data && data.projects.length > limit ? '/projects' : null}>
            {#snippet titleAction()}
              {#if data?.can_create}
                <button type="button" class={titleIconButton} title="New project" aria-label="New project" onclick={() => (creating = true)}>
                  <Reicon name="plus" class="size-3" />
                </button>
              {/if}
            {/snippet}
            {#snippet actions()}
              <ViewToggle single={!projects.length} canvasUrl="/dashboard/canvas" bind:mode={projectsView.mode} />
            {/snippet}
          </SectionHeading>

          {#if data && projects.length === 0}
            <Empty title="No projects yet" description="Create your first deployment workspace." iconName="projects" size="sm" />
          {:else if projects.length}
            {#if projectsView.mode === 'grid'}
              <div class="grid min-w-0 grid-cols-1 gap-3 @lg:grid-cols-2 @4xl:grid-cols-4">
                {#each projects as p (p.uuid)}
                  <article class={card}>
                    <a href={navigateTo(p)} use:link class="absolute inset-0 rounded-xl" aria-label="Open {p.name}"></a>
                    <div class="flex min-w-0 items-start gap-3">
                      <div class={glyph}>{#if p.icon_url}<img src={p.icon_url} alt="{p.name} icon" class="h-full w-full rounded-lg object-cover" />{:else}<Reicon name="projects" class="size-4" />{/if}</div>
                      <div class="min-w-0 flex-1 self-center">
                        <h3 class="truncate text-[13px]! leading-4! font-semibold! text-black dark:text-fg">{p.name}</h3>
                        {#if p.description}
                          <p class="mt-0.5 truncate text-[11px] text-neutral-500 dark:text-fg-faint">{p.description}</p>
                        {/if}
                      </div>
                    </div>
                    <div class="mt-auto flex items-center justify-between gap-3 border-t border-neutral-100 pt-2.5 dark:border-white/[0.06]">
                      <p class="project-card-stats relative z-10 min-w-0">
                        <b>{p.environments.length}</b>
                        {plural(p.environments.length, 'environment', 'environments')}
                        <span class="project-card-stats-dot" aria-hidden="true">&middot;</span>
                        <b>{p.resources}</b>
                        {plural(p.resources, 'resource', 'resources')}
                      </p>
                      <div class="relative z-10 flex shrink-0 items-center gap-0.5">{@render projectActions(p)}</div>
                    </div>
                  </article>
                {/each}
              </div>
            {:else}
              <div class="{tableShell} transition-opacity">
                <div class="grid {projectCols} {tableHead}">
                  <SortHeader label="Project" asc="name-asc" desc="name-desc" current={projectSort} onsort={setProjectSort} server />
                  <SortHeader label="Contents" asc="resources-asc" desc="resources-desc" first="desc" current={projectSort} onsort={setProjectSort} server title="Sort by resource count" />
                  <div class="hidden @2xl:block">
                    <SortHeader label="Description" asc="description-asc" desc="description-desc" current={projectSort} onsort={setProjectSort} server />
                  </div>
                  <span class="text-right">Actions</span>
                </div>
                {#each projects as p (p.uuid)}
                  <div class="relative grid {projectCols} {tableRow}">
                    <a href={navigateTo(p)} use:link class="absolute inset-0" aria-label="Open {p.name}"></a>
                    <div class="flex min-w-0 items-center gap-3">
                      <div class={glyph}>{#if p.icon_url}<img src={p.icon_url} alt="{p.name} icon" class="h-full w-full rounded-lg object-cover" />{:else}<Reicon name="projects" class="size-4" />{/if}</div>
                      <p class="truncate text-[13px] font-semibold text-black dark:text-fg">{p.name}</p>
                    </div>
                    <div class="relative z-10 flex w-fit items-center gap-3 font-medium text-neutral-600 dark:text-fg-dim">
                      <span class="inline-flex items-center gap-1" data-tooltip="Environments" aria-label="Environments">
                        <Reicon name="layers" class="size-3.5 text-neutral-400 dark:text-fg-faint" />
                        {p.environments.length}
                      </span>
                      <span class="inline-flex items-center gap-1" data-tooltip="Resources" aria-label="Resources">
                        <Reicon name="grid" class="size-3.5 text-neutral-400 dark:text-fg-faint" />
                        {p.resources}
                      </span>
                    </div>
                    <p class="hidden truncate text-neutral-500 @2xl:block dark:text-fg-dim">{p.description || '-'}</p>
                    <div class="relative z-10 flex items-center justify-end gap-0.5">{@render projectActions(p)}</div>
                  </div>
                {/each}
              </div>
            {/if}
          {/if}
        </section>

        {#if !data || data.shows_team_sections !== false}
        <section class="@container mb-0! min-w-0">
          <SectionHeading title="Servers" href={data && data.servers.length > limit ? '/servers' : null}>
            {#snippet titleAction()}
              {#if data?.can_create}
                <a href="/servers/new" use:link class={titleIconButton} title="New server" aria-label="New server">
                  <Reicon name="plus" class="size-3" />
                </a>
              {/if}
            {/snippet}
            {#snippet actions()}
              {#if servers.length}<ViewToggle bind:mode={serversView.mode} />{/if}
            {/snippet}
          </SectionHeading>

          {#if data && servers.length === 0}
            {#if data.private_keys === 0}
              <Empty
                title="A private key is required"
                description="Add an SSH private key before connecting your first server."
                iconName="keys"
                size="sm"
              >
                {#snippet contents()}
                  {#if data?.can_create}
                    <a href="/security/private-key" use:link class="button button-highlighted">
                      <Reicon name="plus" class="size-3.5" />
                      Add private key
                    </a>
                  {/if}
                {/snippet}
              </Empty>
            {:else}
              <Empty title="No servers yet" description="Connect infrastructure for your deployments." iconName="servers" size="sm">
                {#snippet contents()}
                  {#if data?.can_create}
                    <a href="/servers/new" use:link class="button button-highlighted">
                      <Reicon name="plus" class="size-3.5" />
                      New server
                    </a>
                  {/if}
                {/snippet}
              </Empty>
            {/if}
          {:else if servers.length}
            {#if serversView.mode === 'grid'}
              <div class="grid min-w-0 grid-cols-1 gap-3 @lg:grid-cols-2 @4xl:grid-cols-4">
                {#each servers as s (s.uuid)}
                  <a href="/server/{s.uuid}" use:link aria-label="Open {s.name}" class={card}>
                    {#if s.is_metrics_enabled}
                      <ServerMetricsChart uuid={s.uuid} data={metrics.charts[s.uuid]} />
                    {/if}
                    <div class="relative z-10 flex min-w-0 items-start gap-3">
                      <div class={glyph}><Reicon name="servers" class="size-4" /></div>
                      <div class="min-w-0 flex-1 self-center">
                        <h3 class="truncate text-[13px]! leading-4! font-semibold! text-black dark:text-fg">{s.name}</h3>
                        {#if s.description}
                          <p class="mt-0.5 truncate text-[11px] text-neutral-500 dark:text-fg-faint">{s.description}</p>
                        {/if}
                      </div>
                      {#if s.status_type !== 'success'}
                        <span
                          data-tooltip={s.status}
                          aria-label="Server status: {s.status}"
                          class="flex size-6 shrink-0 items-center justify-center rounded-md {s.status_type === 'warning'
                            ? 'text-orange-500 dark:text-warning'
                            : 'text-red-500 dark:text-red-400'}"
                        >
                          <Reicon name="alert-triangle" class="size-4" />
                        </span>
                      {/if}
                    </div>
                  </a>
                {/each}
              </div>
            {:else}
              <div class="{tableShell} transition-opacity">
                <div class="grid {serverCols} gap-4 @2xl:min-w-[480px] {tableHead}">
                  <SortHeader label="Server" asc="name-asc" desc="name-desc" current={serverSort} onsort={setServerSort} server />
                  <div class="hidden @2xl:block">
                    <SortHeader label="Description" asc="description-asc" desc="description-desc" current={serverSort} onsort={setServerSort} server />
                  </div>
                  {#if showServerUsage}
                    <div class="hidden @xl:block">Usage</div>
                  {/if}
                  <SortHeader label="Status" asc="status-asc" desc="status-desc" current={serverSort} onsort={setServerSort} server />
                </div>
                {#each servers as s (s.uuid)}
                  <a href="/server/{s.uuid}" use:link class="grid {serverCols} gap-x-4 gap-y-2 @2xl:min-w-[480px] hover:no-underline {tableRow}">
                    <div class="flex min-w-0 items-center gap-3">
                      <div class={glyph}><Reicon name="servers" class="size-4" /></div>
                      <p class="truncate text-[13px] font-semibold text-black dark:text-fg">{s.name}</p>
                    </div>
                    <p class="hidden truncate text-neutral-500 @2xl:block dark:text-fg-dim">{s.description || '-'}</p>
                    {#if showServerUsage}
                      <!-- Below @xl the usage takes a second line of the row, under the name and status. -->
                      <div class="order-last col-span-full @xl:order-none @xl:col-span-1 {s.is_metrics_enabled ? '' : 'hidden @xl:block'}">
                        {#if s.is_metrics_enabled}
                          <div class="grid grid-cols-2 gap-x-4 gap-y-1 text-[11px] text-neutral-500 tabular-nums @xl:grid-cols-1 dark:text-fg-dim">
                            {#each [['CPU', 'bg-[rgb(30,144,255)]', latest(metrics.charts[s.uuid]?.cpu)], ['Mem', 'bg-[rgb(168,85,247)]', latest(metrics.charts[s.uuid]?.memory)]] as [metricLabel, metricColor, value] (metricLabel)}
                              <div class="flex items-center gap-2">
                                <span class="w-7 shrink-0">{metricLabel}</span>
                                <span class="h-1 min-w-0 flex-1 overflow-hidden rounded-full bg-neutral-200 dark:bg-white/[0.08]">
                                  <span class="block h-full rounded-full {metricColor} transition-[width] duration-500" style="width: {Math.min(100, Number(value ?? 0))}%"></span>
                                </span>
                                <span class="w-8 shrink-0 text-right">{value === null ? '-' : `${Math.round(Number(value))}%`}</span>
                              </div>
                            {/each}
                          </div>
                        {:else}
                          <span class="text-neutral-500 dark:text-fg-dim">-</span>
                        {/if}
                      </div>
                    {/if}
                    <div class="flex items-center gap-2 text-[11px] font-medium text-neutral-600 dark:text-fg-dim">
                      <span
                        class="size-2 shrink-0 rounded-full {s.status_type === 'success'
                          ? 'bg-green-500 dark:bg-green-400'
                          : s.status_type === 'warning'
                            ? 'bg-orange-500 dark:bg-warning'
                            : 'bg-red-500 dark:bg-red-400'}"
                      ></span>
                      {s.status}
                    </div>
                  </a>
                {/each}
              </div>
            {/if}
          {/if}
        </section>
        {/if}

        <!-- coolify-rs plugins (ADR 0013): nothing unless the team has a dashboard panel. -->
        <PluginPanels slot="dashboard" />
      </div>
    </div>
  </div>
</div>

{#if creating}
  <NewProjectModal onclose={() => (creating = false)} />
{/if}
