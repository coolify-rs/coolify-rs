<script lang="ts">
  // Port of upstream livewire/project/resource/index.blade.php.
  import Reicon from '../components/Reicon.svelte';
  import Empty from '../components/Empty.svelte';
  import Dropdown from '../components/Dropdown.svelte';
  import SortHeader from '../components/SortHeader.svelte';
  import Pager from '../components/Pager.svelte';
  import ViewToggle from '../components/ViewToggle.svelte';
  import ResourceLogo from '../components/ResourceLogo.svelte';
  import type { ServiceListItem } from '../lib/servicesApi';
  import ResourceStatus, { statusLabel, statusState } from '../components/ResourceStatus.svelte';
  import { uiApi, dbUi, ApiError, type ProjectPageData, type DatabaseRecord, type ApplicationRecord } from '../lib/api';
  import { link } from '../lib/router.svelte';
  import ScanMarker, { type Marker } from '../components/imageScan/ScanMarker.svelte';
  import { listView } from '../lib/listview.svelte';
  import { setProjectCrumbs } from '../lib/crumbs';

  let { uuid, env }: { uuid: string; env: string } = $props();

  let data = $state<ProjectPageData | null>(null);
  let missing = $state(false);
  const environment = $derived(data?.environments.find((e) => e.uuid === env));

  // The environment's resources: null while loading, 'unavailable' when the
  // read model is not there (the page then says resources cannot be shown).
  let resources = $state<{ applications?: ApplicationRecord[]; databases: DatabaseRecord[]; services?: ServiceListItem[]; unported: number } | 'unavailable' | null>(null);

  $effect(() => {
    uiApi
      .project(uuid)
      .then((d) => {
        const e = d.environments.find((x) => x.uuid === env);
        if (!e) {
          missing = true;
          return;
        }
        data = d;
        setProjectCrumbs(d, env);
        document.title = `${e.name.slice(0, 10)} > Resources | coolify-rs`;
      })
      .catch((e) => {
        if (e instanceof ApiError && e.status === 404) missing = true;
        else throw e;
      });
    dbUi
      .resources(uuid, env)
      .then((r) => (resources = r))
      .catch((e) => {
        if (e instanceof ApiError && e.status === 404) resources = 'unavailable';
        else throw e;
      });
  });

  // toSearchableArray() in app/Livewire/Project/Resource/Index.php, for databases.
  interface Item {
    uuid: string;
    name: string;
    type: string;
    typeLabel: string;
    databaseType: string;
    application: { buildPack: string | null; sourceType: string | null; fqdn: string | null; basicAuth: boolean } | null;
    service: { logo: { src: string; fallback: string } | null } | null;
    description: string | null;
    status: string;
    restartLimitReached: boolean;
    restartCount: number;
    maxRestartCount: number;
    hrefLink: string;
    server: string;
    tags: { id: number | string; name: string }[];
    /** `VulnerabilityMarkers::for($resources)[ResourceScans::key($item)] ?? null`. */
    security: Marker | null;
  }

  function toItem(db: DatabaseRecord): Item {
    const limitReached = String(db.status ?? '').startsWith('exited') && db.restart_limit_reached === true;
    const restartCount = Number(db.restart_count ?? 0);
    const maxRestartCount = Number(db.max_restart_count ?? 0);
    const tags = Array.isArray(db.tags) ? (db.tags as { id: number | string; name: string }[]) : [];
    return {
      uuid: db.uuid,
      name: db.name,
      type: 'database',
      typeLabel: 'Database',
      databaseType: db.database_type,
      application: null,
      service: null,
      description: db.description ?? null,
      status: db.status ?? '',
      restartLimitReached: limitReached,
      restartCount: limitReached ? Math.max(restartCount, maxRestartCount) : restartCount,
      maxRestartCount,
      hrefLink: `/project/${uuid}/environment/${env}/database/${db.uuid}`,
      server: db.destination?.server?.name || 'Unknown',
      tags,
      security: ((db as unknown as { security?: Marker | null }).security ?? null),
    };
  }

  function applicationItem(app: ApplicationRecord): Item {
    const limitReached = String(app.status ?? '').startsWith('exited') && app.container_present === true && app.restart_limit_reached === true;
    const restartCount = Number(app.restart_count ?? 0);
    const maxRestartCount = Number(app.max_restart_count ?? 0);
    return {
      uuid: app.uuid,
      name: app.name,
      type: 'application',
      typeLabel: 'Application',
      databaseType: '',
      application: {
        buildPack: app.build_pack ?? null,
        sourceType: app.source_type ?? null,
        fqdn: app.fqdn ?? null,
        basicAuth: app.is_http_basic_auth_enabled === true,
      },
      service: null,
      description: app.description ?? null,
      status: app.status ?? '',
      restartLimitReached: limitReached,
      restartCount: limitReached ? Math.max(restartCount, maxRestartCount) : restartCount,
      maxRestartCount,
      hrefLink: `/project/${uuid}/environment/${env}/application/${app.uuid}`,
      server: app.destination?.server?.name || 'Unknown',
      tags: Array.isArray(app.tags) ? (app.tags as { id: number | string; name: string }[]) : [],
      security: ((app as unknown as { security?: Marker | null }).security ?? null),
    };
  }

  // `$servicesJs`: services are listed after the applications and databases.
  function serviceItem(s: ServiceListItem): Item {
    return {
      uuid: s.uuid,
      name: s.name,
      type: 'service',
      typeLabel: 'Service',
      databaseType: '',
      application: null,
      service: { logo: s.logo },
      description: s.description ?? null,
      status: s.status ?? '',
      restartLimitReached: false,
      restartCount: 0,
      maxRestartCount: 0,
      hrefLink: `/project/${uuid}/environment/${env}/service/${s.uuid}`,
      server: s.server || 'Unknown',
      tags: s.tags ?? [],
      security: ((s as unknown as { security?: Marker | null }).security ?? null),
    };
  }

  const items = $derived(
    resources && resources !== 'unavailable'
      ? [...(resources.applications ?? []).map(applicationItem), ...resources.databases.map(toItem), ...(resources.services ?? []).map(serviceItem)]
      : [],
  );
  // `firstDomain()`, `displayDomain()` of livewire/project/resource/index.blade.php.
  function firstDomain(fqdn: string): string {
    return String(fqdn).split(',')[0].trim();
  }
  function displayDomain(fqdn: string): string {
    return firstDomain(fqdn).replace(/^https?:\/\//, '');
  }

  const unported = $derived(resources && resources !== 'unavailable' ? resources.unported : 0);
  // Upstream's `$environment->isEmpty()`: the resource list when there is one, else the project's count.
  const isEmpty = $derived(
    resources && resources !== 'unavailable' ? items.length + unported === 0 : environment?.resources === 0,
  );

  let search = $state('');
  let typeFilters = $state<string[]>([]);
  let tagFilters = $state<string[]>([]);
  let serverFilters = $state<string[]>([]);
  let statusFilters = $state<string[]>([]);
  let sortBy = $state('name-asc');
  let page = $state(1);
  let pageSize = $state(10);
  const view = listView('environment', 'table');

  type FilterKey = 'typeFilters' | 'tagFilters' | 'serverFilters' | 'statusFilters';
  type Option = { value: string; label: string };

  function uniqueOptions(options: Option[]): Option[] {
    return [...new Map(options.filter((o) => o.value).map((o) => [o.value, o])).values()].sort((a, b) => a.label.localeCompare(b.label));
  }

  const filterGroups = $derived<{ key: FilterKey; label: string; options: Option[] }[]>([
    { key: 'typeFilters', label: 'Resource types', options: uniqueOptions(items.map((i) => ({ value: i.type, label: i.typeLabel }))) },
    { key: 'tagFilters', label: 'Tags', options: uniqueOptions(items.flatMap((i) => i.tags.map((t) => ({ value: t.name, label: t.name })))) },
    { key: 'serverFilters', label: 'Servers', options: uniqueOptions(items.map((i) => ({ value: i.server, label: i.server }))) },
    { key: 'statusFilters', label: 'Statuses', options: uniqueOptions(items.map((i) => ({ value: statusState(i), label: statusLabel(i) }))) },
  ]);

  function selected(key: FilterKey): string[] {
    return { typeFilters, tagFilters, serverFilters, statusFilters }[key];
  }
  function setSelected(key: FilterKey, value: string[]) {
    if (key === 'typeFilters') typeFilters = value;
    else if (key === 'tagFilters') tagFilters = value;
    else if (key === 'serverFilters') serverFilters = value;
    else statusFilters = value;
  }
  function toggleFilter(key: FilterKey, value: string) {
    const current = selected(key);
    setSelected(key, current.includes(value) ? current.filter((v) => v !== value) : [...current, value]);
    page = 1;
  }
  function clearFilters() {
    typeFilters = [];
    tagFilters = [];
    serverFilters = [];
    statusFilters = [];
    page = 1;
  }

  const activeFilterCount = $derived(typeFilters.length + tagFilters.length + serverFilters.length + statusFilters.length);
  const filterButtonText = $derived.by(() => {
    const labels = filterGroups.flatMap((g) => g.options.filter((o) => selected(g.key).includes(o.value)).map((o) => o.label));
    if (labels.length === 0) return 'Filter';
    if (labels.length === 1) return labels[0];
    return `${labels[0]} +${labels.length - 1}`;
  });

  const sortOptions = [
    { value: 'name-asc', label: 'Name A–Z' },
    { value: 'name-desc', label: 'Name Z–A' },
    { value: 'type-asc', label: 'Resource type' },
    { value: 'status-asc', label: 'Status' },
  ];
  const sortResources = (value: string) => ((sortBy = value), (page = 1));

  const filtered = $derived.by(() => {
    const query = search.trim().toLowerCase();
    const list = items.filter((item) => {
      const matchesType = typeFilters.length === 0 || typeFilters.includes(item.type);
      const matchesTags = tagFilters.length === 0 || item.tags.some((t) => tagFilters.includes(t.name));
      const matchesServer = serverFilters.length === 0 || serverFilters.includes(item.server);
      const matchesStatus = statusFilters.length === 0 || statusFilters.includes(statusState(item));
      const searchable = [item.name, item.description, item.application?.fqdn, item.typeLabel, item.status, item.server, ...item.tags.map((t) => t.name)]
        .filter(Boolean)
        .join(' ')
        .toLowerCase();
      return matchesType && matchesTags && matchesServer && matchesStatus && (!query || searchable.includes(query));
    });
    const [column, direction] = sortBy.split('-');
    type Item = (typeof list)[number];
    const sortValues: Record<string, (i: Item) => string> = {
      name: (i) => i.name,
      type: (i) => i.typeLabel,
      status: (i) => statusLabel(i),
      domain: (i) => (i.application?.fqdn ? displayDomain(i.application.fqdn) : '') || '',
      server: (i) => i.server || 'Unknown',
    };
    const sortValue = sortValues[column] ?? sortValues.name;
    const order = direction === 'desc' ? -1 : 1;
    return list.sort((a, b) => order * (sortValue(a).localeCompare(sortValue(b)) || a.name.localeCompare(b.name)));
  });
  const totalPages = $derived(Math.max(1, Math.ceil(filtered.length / pageSize)));
  $effect.pre(() => {
    if (page > totalPages) page = totalPages;
  });
  const paginated = $derived(filtered.slice((page - 1) * pageSize, page * pageSize));

  const tagLink =
    'relative max-w-24 truncate rounded-md border border-neutral-200 bg-neutral-50 px-1.5 py-0.5 text-[10px] text-neutral-500 hover:text-black dark:border-white/[0.08] dark:bg-white/[0.035] dark:text-fg-faint dark:hover:text-fg';
</script>

{#snippet noMatches(cls: string)}
  <div class="flex min-h-52 flex-col items-center justify-center px-6 text-center {cls}">
    <Reicon name="search" class="mb-3 size-6 text-neutral-300 dark:text-fg-faint" />
    <p class="text-[13px] font-medium">No matching resources</p>
    <p class="mt-1 text-[12px] text-neutral-500 dark:text-fg-dim">Try a different search or filter.</p>
  </div>
{/snippet}

{#snippet unportedNote()}
  {#if unported > 0}
    <p class="mt-3 flex items-center gap-1.5 text-[12px] text-neutral-500 dark:text-fg-dim">
      <Reicon name="info-circle" class="size-3.5 shrink-0 text-neutral-400 dark:text-fg-faint" />
      {unported}
      {unported === 1 ? 'other resource is' : 'other resources are'} not available in coolify-rs yet. Manage {unported === 1 ? 'it' : 'them'} in Coolify
      for now.
    </p>
  {/if}
{/snippet}

{#if missing}
  <div class="application-settings-form w-full">
    <Empty title="Environment not found" description="It may have been deleted, or it belongs to another team." iconName="layers" size="lg" />
  </div>
{:else if data && environment}
  <div>
    <div class="w-full">
      <header class="mb-5 flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
        <div class="min-w-0">
          <div class="flex min-w-0 items-center gap-1.5">
            <h1 class="truncate text-[24px]! leading-7! font-semibold! tracking-tight!">{environment.name}</h1>
            {#if environment?.can_update ?? data.can_update}
              <a
                href="/project/{uuid}/environment/{env}/edit"
                use:link
                class="flex size-7 shrink-0 items-center justify-center rounded-md text-neutral-400 transition-colors hover:bg-neutral-100 hover:text-black dark:text-fg-faint dark:hover:bg-white/[0.06] dark:hover:text-fg"
                title="Environment settings"
                aria-label="Open settings for {environment.name}"
              >
                <Reicon name="settings" class="size-4" />
              </a>
            {/if}
          </div>
          <p class="mt-1 text-[13px] text-neutral-500 dark:text-fg-dim">
            <span>{environment.resources} {environment.resources === 1 ? 'resource' : 'resources'}</span>
            in
            <a
              href="/project/{uuid}"
              use:link
              class="font-medium text-neutral-700 underline-offset-2 hover:text-black hover:underline dark:text-fg dark:hover:text-white"
              title="Show all environments in {data.project.name}">{data.project.name}</a
            >
          </p>
        </div>
        <div class="flex flex-wrap items-center gap-2 sm:shrink-0">
          {#if isEmpty}<ViewToggle single canvasUrl="/project/{uuid}/environment/{env}/canvas" />{/if}
          <a
            href="/shared-variables/environments/project/{uuid}/environment/{env}"
            use:link
            class="button whitespace-nowrap"
            title="Shared variables for this environment"
            aria-label="Shared variables for {environment.name}"
          >
            <Reicon name="variables" class="size-3.5" />
            Shared variables
          </a>
          {#if environment?.can_create ?? data.can_create}
            <a href="/project/{uuid}/environment/{env}/new" use:link class="button whitespace-nowrap button-highlighted">
              <Reicon name="plus" class="size-3.5" />
              New resource
            </a>
          {/if}
        </div>
      </header>

      {#if isEmpty}
        <Empty title="No resources yet" description="Add an application, database, or service to this environment." iconName="layers">
          {#snippet contents()}
            {#if environment?.can_create ?? data?.can_create}
              <a href="/project/{uuid}/environment/{env}/new" use:link class="button">
                <Reicon name="plus" class="size-3.5" />
                Add resource
              </a>
            {/if}
          {/snippet}
        </Empty>
      {:else if resources === 'unavailable' || (resources && items.length === 0)}
        <Empty
          title="Resources are not available in coolify-rs yet"
          description="This environment has resources. Manage them in Coolify for now."
          iconName="layers"
        />
      {:else if resources}
        <div class="mb-3 flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
          <div class="relative w-full sm:max-w-sm">
            <Reicon
              name="search"
              class="pointer-events-none absolute top-1/2 left-2.5 z-10 size-3.5 -translate-y-1/2 text-neutral-400 dark:text-fg-faint"
            />
            <input
              bind:value={search}
              oninput={() => (page = 1)}
              type="search"
              placeholder="Search resources"
              class="h-8! w-full rounded-lg! border-neutral-200! bg-white! py-0! pr-8! pl-8! text-[12px]! shadow-none! placeholder:text-neutral-400 focus:border-accent! focus:ring-0! dark:border-white/[0.08]! dark:bg-white/[0.035]! dark:text-fg! dark:placeholder:text-fg-faint"
            />
            {#if search}
              <button
                onclick={() => ((search = ''), (page = 1))}
                type="button"
                class="absolute top-1/2 right-2 flex size-5 -translate-y-1/2 items-center justify-center rounded text-neutral-400 transition-colors hover:bg-neutral-100 hover:text-black dark:text-fg-faint dark:hover:bg-white/[0.07] dark:hover:text-fg"
                aria-label="Clear search"
              >
                <span class="text-sm leading-none">×</span>
              </button>
            {/if}
          </div>

          <div class="flex flex-wrap items-center gap-2">
            <Dropdown panelClass="w-64! overflow-hidden! p-0!">
              {#snippet trigger({ open })}
                <button
                  type="button"
                  class="button max-w-64 {activeFilterCount > 0 ? 'button-highlighted' : ''}"
                  title={activeFilterCount > 0 ? filterButtonText : 'Filter'}
                  aria-haspopup="listbox"
                  aria-expanded={open}
                >
                  <svg class="size-3.5 opacity-65" viewBox="0 0 24 24" fill="none" aria-hidden="true">
                    <path d="M4 6h16M7 12h10M10 18h4" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" />
                  </svg>
                  <span class="truncate">{activeFilterCount > 0 ? filterButtonText : 'Filter'}</span>
                  {#if activeFilterCount > 0}
                    <span
                      class="shrink-0 rounded-full bg-neutral-100 px-1.5 py-0.5 text-[10px] font-medium text-neutral-500 dark:bg-white/[0.07] dark:text-fg-dim"
                      >{activeFilterCount}</span
                    >
                  {/if}
                </button>
              {/snippet}
              {#snippet children()}
                <div class="max-h-80 overflow-y-auto p-1">
                  {#each filterGroups as group (group.key)}
                    {#if group.options.length > 0}
                      <div>
                        <div class="px-2 pt-2 pb-1 text-[10px] font-semibold uppercase tracking-wide text-neutral-400 dark:text-fg-faint">
                          {group.label}
                        </div>
                        {#each group.options as option (`${group.key}-${option.value}`)}
                          {@const on = selected(group.key).includes(option.value)}
                          <button type="button" class="listbox-option" onclick={() => toggleFilter(group.key, option.value)}>
                            <span class="min-w-0 flex-1 truncate">{option.label}</span>
                            <span
                              class="flex size-4 shrink-0 items-center justify-center rounded-[5px] border {on
                                ? 'border-coollabs bg-coollabs text-white dark:border-brand dark:bg-brand dark:text-white'
                                : 'border-neutral-300 bg-white dark:border-white/[0.14] dark:bg-white/[0.045]'}"
                            >
                              {#if on}
                                <svg class="size-3" viewBox="0 0 12 12" fill="none" aria-hidden="true">
                                  <path
                                    d="m2.25 6.15 2.35 2.3 5.15-5"
                                    stroke="currentColor"
                                    stroke-width="1.8"
                                    stroke-linecap="round"
                                    stroke-linejoin="round"
                                  />
                                </svg>
                              {/if}
                            </span>
                          </button>
                        {/each}
                      </div>
                    {/if}
                  {/each}
                </div>
                <div class="border-t border-neutral-200 bg-white p-1 dark:border-white/10 dark:bg-raised">
                  <button type="button" class="listbox-option justify-center! text-center!" onclick={clearFilters}>Clear filters</button>
                </div>
              {/snippet}
            </Dropdown>

            <div class={view.mode === 'grid' ? '' : 'min-[769px]:hidden'}>
            <Dropdown panelClass="w-48!">
              {#snippet trigger({ open })}
                <button type="button" class="button" aria-haspopup="listbox" aria-expanded={open}>
                  <svg class="size-3.5 opacity-65" viewBox="0 0 24 24" fill="none" aria-hidden="true">
                    <path
                      d="M8 5v14m0 0-3-3m3 3 3-3M16 19V5m0 0-3 3m3-3 3 3"
                      stroke="currentColor"
                      stroke-width="1.7"
                      stroke-linecap="round"
                      stroke-linejoin="round"
                    />
                  </svg>
                  Sort
                </button>
              {/snippet}
              {#snippet children({ close })}
                {#each sortOptions as option (option.value)}
                  <button
                    type="button"
                    class="listbox-option"
                    onclick={() => ((sortBy = option.value), close(), (page = 1))}
                  >
                    <span class="flex-1">{option.label}</span>
                    {#if sortBy === option.value}
                      <svg class="size-3.5 text-black dark:text-fg" viewBox="0 0 12 12" fill="none" aria-hidden="true">
                        <path d="m2.5 6.25 2.1 2.1 4.9-5" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round" />
                      </svg>
                    {/if}
                  </button>
                {/each}
              {/snippet}
            </Dropdown>
            </div>

            <ViewToggle canvasUrl="/project/{uuid}/environment/{env}/canvas" bind:mode={() => view.mode, (m) => ((view.mode = m), (page = 1))} />
          </div>
        </div>

        {#if view.mode === 'grid'}
          <div>
            <div class="grid grid-cols-1 gap-3 sm:grid-cols-2 lg:grid-cols-4">
              {#each paginated as item (item.uuid)}
                <article
                  class="group relative flex min-h-28 flex-col rounded-xl border border-neutral-200 bg-white p-3 shadow-sm transition-all hover:-translate-y-px hover:border-neutral-300 hover:shadow-md dark:border-white/[0.08] dark:bg-white/[0.05] dark:hover:border-white/[0.14]"
                >
                  <a href={item.hrefLink} use:link class="absolute inset-0 rounded-xl" aria-label="Open {item.name}"></a>
                  <div class="flex items-start gap-3">
                    <div
                      class="flex size-8 shrink-0 items-center justify-center overflow-hidden rounded-lg border border-neutral-200 bg-neutral-50 text-neutral-500 dark:border-white/[0.08] dark:bg-white/[0.04] dark:text-fg-dim"
                    >
                      <ResourceLogo databaseType={item.databaseType} application={item.application} service={item.service} />
                    </div>
                    <div class="min-w-0 flex-1">
                      <div class="flex min-w-0 items-center gap-1.5">
                        <h2 class="truncate text-[13px]! leading-4! font-semibold! text-black dark:text-fg">{item.name}</h2>
                        <ScanMarker marker={item.security} class="relative" />
                      </div>
                      <p class="mt-0.5 text-[11px] text-neutral-500 dark:text-fg-faint">{item.typeLabel}</p>
                    </div>
                    <ResourceStatus {item} />
                  </div>
                  <div class="mt-auto flex min-w-0 flex-col gap-0.5 pt-4">
                    <p class="min-h-4 truncate text-[11px] text-neutral-500 dark:text-fg-faint">
                      {#if item.description}<span>{item.description}</span>{/if}
                    </p>
                    {#if item.application?.fqdn}
                      <a href={firstDomain(item.application.fqdn)} target="_blank" rel="noopener noreferrer" class="relative z-10 max-w-full self-start truncate text-[11px] text-neutral-500 hover:underline dark:text-fg-dim" title={displayDomain(item.application.fqdn)}>{displayDomain(item.application.fqdn)}</a>
                    {/if}
                  </div>
                </article>
              {/each}
            </div>

            {#if filtered.length === 0}
              {@render noMatches(
                'rounded-xl border border-neutral-200 bg-white dark:border-white/[0.08] dark:bg-white/[0.05]',
              )}
            {:else}
              <Pager
                total={filtered.length}
                bind:page
                bind:pageSize
                storageKey="coolify.page-size.environment-resources"
                class="mt-3 rounded-xl border border-neutral-200 bg-white shadow-sm dark:border-white/[0.08] dark:bg-white/[0.05]"
              />
            {/if}
          </div>
        {:else}
          <div class="overflow-hidden rounded-xl border border-neutral-200 bg-white shadow-sm dark:border-white/[0.08] dark:bg-white/[0.05]">
            <div
              class="environment-resource-grid border-b border-neutral-200 bg-neutral-50 px-4 py-2.5 text-[11px] font-medium text-neutral-500 dark:border-white/[0.08] dark:bg-white/[0.05] dark:text-fg-faint"
            >
              <SortHeader label="Resource" asc="name-asc" desc="name-desc" current={sortBy} onsort={sortResources} />
              <SortHeader label="Type" asc="type-asc" desc="type-desc" current={sortBy} onsort={sortResources} class="resource-type" />
              <SortHeader label="Status" asc="status-asc" desc="status-desc" current={sortBy} onsort={sortResources} />
              <SortHeader label="Domain" asc="domain-asc" desc="domain-desc" current={sortBy} onsort={sortResources} class="resource-domain" />
              <SortHeader label="Server" asc="server-asc" desc="server-desc" current={sortBy} onsort={sortResources} class="resource-server" />
              <div class="resource-tags">Tags</div>
            </div>

            {#each paginated as item (item.uuid)}
              <div
                class="environment-resource-grid group relative min-h-14 items-center border-b border-neutral-200 px-4 py-2.5 transition-colors last:border-b-0 hover:bg-neutral-50 dark:border-white/[0.07] dark:hover:bg-white/[0.025]"
              >
                <a href={item.hrefLink} use:link class="absolute inset-0" aria-label="Open {item.name}"></a>
                <div class="flex min-w-0 items-center gap-3">
                  <div
                    class="flex size-8 shrink-0 items-center justify-center overflow-hidden rounded-lg border border-neutral-200 bg-neutral-50 text-neutral-500 dark:border-white/[0.08] dark:bg-white/[0.035] dark:text-fg-dim"
                  >
                    <ResourceLogo databaseType={item.databaseType} application={item.application} service={item.service} />
                  </div>
                  <div class="min-w-0">
                    <div class="flex min-w-0 items-center gap-1.5">
                      <a
                        href={item.hrefLink}
                        use:link
                        class="relative block truncate text-[13px] font-semibold text-black hover:underline dark:text-fg">{item.name}</a
                      >
                      <ScanMarker marker={item.security} class="relative" />
                    </div>
                    {#if item.description?.trim()}
                      <p class="truncate text-[11px] text-neutral-500 dark:text-fg-faint">{item.description}</p>
                    {/if}
                    <div class="mobile-resource-domain min-w-0">
                      {#if item.application?.fqdn}
                        <a href={firstDomain(item.application.fqdn)} target="_blank" rel="noopener noreferrer" class="relative z-10 block truncate text-[11px] text-neutral-500 hover:underline dark:text-fg-dim">{displayDomain(item.application.fqdn)}</a>
                      {/if}
                    </div>
                  </div>
                </div>

                <div class="resource-type min-w-0">
                  <button
                    type="button"
                    class="table-filter-value truncate text-[12px] text-neutral-600 dark:text-fg-dim"
                    onclick={() => toggleFilter('typeFilters', item.type)}
                    title="Filter by {item.typeLabel}">{item.typeLabel}</button
                  >
                </div>

                <div class="min-w-0">
                  <ResourceStatus {item} onfilter={() => toggleFilter('statusFilters', statusState(item))} />
                </div>

                <div class="resource-domain min-w-0">
                  {#if item.application?.fqdn}
                    <a href={firstDomain(item.application.fqdn)} target="_blank" class="relative inline-block max-w-full truncate align-middle text-[12px] text-neutral-600 hover:underline dark:text-fg-dim">{displayDomain(item.application.fqdn)}</a>
                  {:else}
                    <span class="text-[12px] text-neutral-400 dark:text-fg-faint">-</span>
                  {/if}
                </div>

                <div class="resource-server min-w-0">
                  <button
                    type="button"
                    class="table-filter-value truncate text-[12px] text-neutral-600 dark:text-fg-dim"
                    onclick={() => toggleFilter('serverFilters', item.server || 'Unknown')}
                    title="Filter by {item.server || 'Unknown'}">{item.server || 'Unknown'}</button
                  >
                </div>

                <div class="resource-tags flex min-w-0 items-center gap-1 overflow-hidden">
                  {#each item.tags.slice(0, 2) as tag (tag.id)}
                    <a href="/tags/{tag.name}" use:link class={tagLink}>{tag.name}</a>
                  {/each}
                  {#if item.tags.length > 2}
                    <span class="text-[10px] text-neutral-400 dark:text-fg-faint">+{item.tags.length - 2}</span>
                  {/if}
                  {#if item.tags.length === 0}
                    <span class="text-[12px] text-neutral-400 dark:text-fg-faint">-</span>
                  {/if}
                </div>
              </div>
            {/each}

            {#if filtered.length === 0}
              {@render noMatches('')}
            {:else}
              <Pager total={filtered.length} bind:page bind:pageSize storageKey="coolify.page-size.environment-resources" />
            {/if}
          </div>
        {/if}

        {@render unportedNote()}
      {/if}
    </div>
  </div>
{/if}
