<script lang="ts">
  // Port of upstream livewire/global-search.blade.php and App\Livewire\GlobalSearch: the command
  // palette, its keyboard shortcuts, the search, the "new ..." commands and the selection steps.
  // `S3 storage` and `Private key` open their page's modal; GitHub App and Canvas go to their page.
  import { tick } from 'svelte';
  import NewProjectModal from './NewProjectModal.svelte';
  import NewTeamModal from './NewTeamModal.svelte';
  import Reicon from './Reicon.svelte';
  import { ApiError } from '../lib/api';
  import { creatableItems, exactMatchCommands, layoutUi, navigationItems, type CreatableItem, type SearchItem } from '../lib/layoutApi';
  import { router } from '../lib/router.svelte';
  import { session } from '../lib/session.svelte';
  import { notify } from '../lib/toast.svelte';

  interface PageItem {
    label: string;
    breadcrumb: string;
    search_text: string;
    href: string;
    element?: HTMLElement;
  }

  let modalOpen = $state(false);
  let selectedIndex = $state(-1);
  let loading = $state(false);
  let showSpinner = $state(false);
  let transitioning = $state(false);
  let searchQuery = $state('');
  let allItems = $state<SearchItem[]>([]);
  let creatable = $state<CreatableItem[]>([]);
  let pageItems = $state<PageItem[]>([]);
  let scope = $state<'global' | 'page'>('global');
  let input: HTMLInputElement | undefined = $state();
  let root: HTMLElement | undefined = $state();
  let spinnerTimer: ReturnType<typeof setTimeout>;

  // The modals of `open-create-modal-*`.
  let createModal = $state<'project' | 'team' | null>(null);

  // Selection state of `navigateToResourceCreation`.
  let selecting = $state(false);
  let selectedType = $state<string | null>(null);
  let selectedServer = $state<number | null>(null);
  let selectedDestination = $state<string | null>(null);
  let selectedProject = $state<string | null>(null);
  let selectedEnvironment = $state<string | null>(null);
  let servers = $state<any[]>([]);
  let destinations = $state<any[]>([]);
  let projects = $state<any[]>([]);
  let environments = $state<any[]>([]);

  const selectedName = $derived(creatable.find((i) => i.type === selectedType)?.name ?? null);

  const pageResults = $derived.by(() => {
    const q = searchQuery.toLowerCase().trim();
    if (scope !== 'page' && !q) return [];
    const r = pageItems.filter((i) => i.search_text.toLowerCase().includes(q));
    return scope === 'page' ? r : r.slice(0, 5);
  });

  const searchResults = $derived.by(() => {
    if (scope === 'page' || !searchQuery || loading) return [];
    const q = searchQuery.toLowerCase().trim();
    return allItems.filter((i) => i.search_text && i.search_text.toLowerCase().includes(q)).slice(0, 20);
  });

  const filteredCreatable = $derived.by(() => {
    if (scope === 'page' || !searchQuery || loading) return [];
    const q = searchQuery.toLowerCase().trim();
    if (q === 'new') return creatable;
    return creatable.filter((i) => {
      const text = `${i.name} ${i.description} ${i.type} ${i.category}`.toLowerCase();
      if (q.startsWith('new ')) {
        const rest = q.substring(4);
        return text.includes(rest) || text.includes(q);
      }
      return text.includes(q);
    });
  });

  const grouped = $derived.by(() => {
    const g: Record<string, CreatableItem[]> = {};
    for (const i of filteredCreatable) (g[i.category || 'Other'] ??= []).push(i);
    return Object.entries(g);
  });

  /** `window.currentPageSearchItems`: the sections of the open page. */
  function currentPageItems(): PageItem[] {
    const out: PageItem[] = [];
    const selector = ':scope > header :is(h1, h2, h3), :scope > .application-settings-section-header :is(h1, h2, h3)';
    document.querySelectorAll<HTMLElement>('.application-settings-section').forEach((section, index) => {
      if (section.closest('[role=dialog], .command-palette') || section.getClientRects().length === 0) return;
      const label = (section.dataset.settingsSectionTitle ?? section.querySelector(selector)?.textContent ?? '').replace(/\s+/g, ' ').trim();
      if (!label) return;
      out.push({ label, breadcrumb: 'Section', search_text: `${label} section`, href: `section-${index}`, element: section });
    });
    return out;
  }

  async function openModal() {
    clearTimeout(spinnerTimer);
    modalOpen = true;
    selectedIndex = -1;
    loading = true;
    showSpinner = false;
    searchQuery = '';
    pageItems = currentPageItems();
    scope = 'global';
    spinnerTimer = setTimeout(() => {
      if (loading) showSpinner = true;
    }, 150);
    try {
      const d = await layoutUi.searchItems();
      const ctx = { isAdmin: session.isAdmin, isOwner: session.isOwner, isInstanceAdmin: session.isInstanceAdmin };
      allItems = [...navigationItems(ctx, session.info?.current_team?.id), ...d.resources];
      creatable = creatableItems(session.isAdmin, d.services);
    } catch {
      modalOpen = false;
    }
    clearTimeout(spinnerTimer);
    loading = false;
    showSpinner = false;
    setTimeout(() => input?.focus(), 50);
  }

  function closeModal() {
    modalOpen = false;
    selectedIndex = -1;
    setTimeout(() => {
      loading = false;
      showSpinner = false;
      searchQuery = '';
      allItems = [];
      transitioning = false;
    }, 150);
    cancelSelection();
  }

  function setScope(s: 'global' | 'page') {
    scope = s;
    selectedIndex = -1;
    input?.focus();
  }

  function visibleResults(): HTMLElement[] {
    return Array.from(root?.querySelectorAll<HTMLElement>('.search-result-item') ?? []).filter((i) => i.offsetParent !== null);
  }

  function navigateResults(direction: 'up' | 'down') {
    const results = visibleResults();
    if (results.length === 0) return;
    selectedIndex = direction === 'down' ? Math.min(selectedIndex + 1, results.length - 1) : Math.max(selectedIndex - 1, -1);
    if (selectedIndex >= 0) {
      results[selectedIndex].focus();
      results[selectedIndex].scrollIntoView({ block: 'nearest' });
    } else {
      input?.focus();
    }
  }

  async function preselectFirst() {
    await tick();
    const results = visibleResults();
    if (results.length === 0) {
      selectedIndex = -1;
      return;
    }
    selectedIndex = 0;
    results[0].focus();
    results[0].scrollIntoView({ block: 'nearest' });
  }

  function onQueryInput() {
    selectedIndex = -1;
    const trimmed = searchQuery.trim().toLowerCase();
    if (trimmed === '') {
      if (selecting) cancelSelection();
      return;
    }
    if (scope === 'global' && exactMatchCommands.includes(trimmed)) {
      const match = creatable.find((i) => {
        const byName = `new ${i.name}`.toLowerCase();
        const byType = `new ${i.type}`.toLowerCase();
        const byTypeSpaces = i.type ? `new ${i.type.replace(/-/g, ' ')}` : '';
        return byName === trimmed || byType === trimmed || byTypeSpaces === trimmed || (i.quickcommand && i.quickcommand.toLowerCase().includes(trimmed));
      });
      if (match) navigateToResource(match.type);
    }
  }

  function openPageItem(item: PageItem) {
    closeModal();
    if (item.element) {
      if (item.element.id) history.replaceState(null, '', `#${item.element.id}`);
      item.element.scrollIntoView({ behavior: 'smooth', block: 'start' });
    }
  }

  /** `navigateToResource`. */
  async function navigateToResource(type: string) {
    if (type === 'server') {
      closeModal();
      router.go('/servers/new');
      return;
    }
    const item = creatable.find((i) => i.type === type);
    if (!item) return;
    if (item.route) {
      closeModal();
      router.go(`/${item.route}`);
      return;
    }
    if (item.component) {
      closeModal();
      if (type === 'project' || type === 'team') {
        createModal = type;
      } else if (type === 'storage') {
        router.go('/storages?create=1');
      } else if (type === 'private-key') {
        router.go('/security/private-key?create=1');
      } else {
        router.go('/sources');
      }
      return;
    }
    if (item.resourceType) {
      selectedType = type;
      selecting = true;
      searchQuery = '';
      selectedServer = null;
      selectedDestination = null;
      selectedProject = null;
      selectedEnvironment = null;
      transitioning = true;
      await loadServers().finally(() => (transitioning = false));
    }
  }

  async function step(name: string, id = ''): Promise<any[] | null> {
    try {
      return (await layoutUi.step(name, id)).items;
    } catch (e) {
      notify.failure(e instanceof ApiError ? e.message : String(e));
      return null;
    }
  }

  async function loadServers() {
    servers = (await step('servers')) ?? [];
    if (servers.length === 1) await selectServer(servers[0].id);
    else preselectFirst();
  }

  async function selectServer(id: number) {
    selectedServer = id;
    const list = await step('destinations', String(id));
    if (!list) return;
    destinations = list;
    if (list.length === 1) await selectDestination(list[0].uuid);
    else preselectFirst();
  }

  async function selectDestination(uuid: string) {
    selectedDestination = uuid;
    const list = await step('projects');
    if (!list) return;
    projects = list;
    if (list.length === 1) await selectProject(list[0].uuid);
    else preselectFirst();
  }

  async function selectProject(uuid: string) {
    selectedProject = uuid;
    const list = await step('environments', uuid);
    if (!list) return;
    environments = list;
    if (list.length === 1) selectEnvironment(list[0].uuid);
    else preselectFirst();
  }

  /** `completeResourceCreation`. */
  function selectEnvironment(uuid: string) {
    selectedEnvironment = uuid;
    if (selectedProject && selectedType && selectedServer !== null && selectedDestination) {
      const q = new URLSearchParams({ type: selectedType, destination: selectedDestination, server_id: String(selectedServer) });
      const to = `/project/${selectedProject}/environment/${uuid}/new?${q}`;
      closeModal();
      router.go(to);
    }
  }

  function cancelSelection() {
    selecting = false;
    selectedType = null;
    selectedServer = null;
    selectedDestination = null;
    selectedProject = null;
    selectedEnvironment = null;
    servers = [];
    destinations = [];
    projects = [];
    environments = [];
  }

  /** `goBack`. */
  function goBack() {
    if (selectedProject !== null) {
      selectedProject = null;
      selectedEnvironment = null;
      if (projects.length > 1) return;
    }
    if (selectedDestination !== null) {
      selectedDestination = null;
      selectedProject = null;
      selectedEnvironment = null;
      if (destinations.length > 1) return;
    }
    if (selectedServer !== null) {
      selectedServer = null;
      selectedDestination = null;
      selectedProject = null;
      selectedEnvironment = null;
      if (servers.length > 1) return;
    }
    cancelSelection();
  }

  function onKeydown(e: KeyboardEvent) {
    const target = e.target as HTMLElement;
    if (e.key === '/' && !['INPUT', 'TEXTAREA'].includes(target.tagName)) {
      e.preventDefault();
      if (!modalOpen) openModal();
      else {
        input?.focus();
        selectedIndex = -1;
      }
      return;
    }
    if ((e.metaKey || e.ctrlKey) && e.key === 'k') {
      e.preventDefault();
      if (modalOpen) {
        input?.focus();
        selectedIndex = -1;
      } else openModal();
      return;
    }
    if (!modalOpen) return;
    if (e.key === 'Escape') {
      if (!searchQuery) {
        if (selecting) {
          cancelSelection();
          setTimeout(() => input?.focus(), 100);
        } else closeModal();
      } else {
        searchQuery = '';
        setTimeout(() => input?.focus(), 100);
      }
    } else if (e.key === 'ArrowDown') {
      e.preventDefault();
      navigateResults('down');
    } else if (e.key === 'ArrowUp') {
      e.preventDefault();
      navigateResults('up');
    } else if (e.key === 'Tab' && pageItems.length > 0 && document.activeElement === input) {
      e.preventDefault();
      setScope(scope === 'page' ? 'global' : 'page');
    }
  }

  const stepTitle = $derived(
    selectedServer === null
      ? 'Select server'
      : selectedDestination === null
        ? 'Select destination'
        : selectedProject === null
          ? 'Select project'
          : 'Select environment',
  );
  const stepItems = $derived(selectedServer === null ? servers : selectedDestination === null ? destinations : selectedProject === null ? projects : environments);
  const stepEmpty = $derived(
    selectedServer === null ? 'No servers available' : selectedDestination === null ? 'No destinations available' : selectedProject === null ? 'No projects available' : 'No environments available',
  );

  function pickStep(item: any) {
    if (selectedServer === null) selectServer(item.id);
    else if (selectedDestination === null) selectDestination(item.uuid);
    else if (selectedProject === null) selectProject(item.uuid);
    else selectEnvironment(item.uuid);
  }

  const typeLabel = (r: SearchItem): string => {
    switch (r.type) {
      case 'navigation':
        return 'Navigation';
      case 'application':
        return 'Application';
      case 'service':
        return 'Service';
      case 'database':
        return r.subtype ? r.subtype.charAt(0).toUpperCase() + r.subtype.slice(1) : 'Database';
      case 'server':
        return 'Server';
      case 'project':
        return 'Project';
      case 'environment':
        return 'Environment';
      default:
        return '';
    }
  };
</script>

<svelte:window onopen-global-search={openModal} onkeydown={onKeydown} />

<div bind:this={root}>
  <div class="fixed inset-0 z-99 flex items-start justify-center px-4 pt-[12vh] {modalOpen ? 'pointer-events-auto' : 'pointer-events-none'}">
    {#if modalOpen}
      <div role="presentation" onclick={closeModal} class="absolute inset-0 w-full h-full bg-black/50 backdrop-blur-[2px]"></div>
      <div class="command-palette relative mx-auto">
        <div class="command-palette-header">
          <span class="command-palette-header-icon {showSpinner ? 'is-loading' : ''}">
            {#if !showSpinner}
              <svg class="size-4" viewBox="0 0 24 24" fill="none" aria-hidden="true">
                <path
                  fill-rule="evenodd"
                  clip-rule="evenodd"
                  d="M16.489 17.5497C14.8753 18.922 12.7843 19.75 10.5 19.75C5.39137 19.75 1.25 15.6086 1.25 10.5C1.25 5.39137 5.39137 1.25 10.5 1.25C15.6086 1.25 19.75 5.39137 19.75 10.5C19.75 12.7843 18.922 14.8753 17.5497 16.489L22.5303 21.4697C22.8232 21.7626 22.8232 22.2374 22.5303 22.5303C22.2374 22.8232 21.7626 22.8232 21.4697 22.5303L16.489 17.5497ZM2.75 10.5C2.75 6.21979 6.21979 2.75 10.5 2.75C14.7802 2.75 18.25 6.21979 18.25 10.5C18.25 12.589 17.4235 14.485 16.0796 15.8788C16.0408 15.905 16.004 15.9353 15.9697 15.9697C15.9353 16.004 15.905 16.0408 15.8788 16.0796C14.485 17.4235 12.589 18.25 10.5 18.25C6.21979 18.25 2.75 14.7802 2.75 10.5Z"
                  fill="currentColor"
                />
              </svg>
            {:else}
              <svg class="size-4 animate-spin" viewBox="0 0 24 24" fill="none" aria-hidden="true">
                <circle class="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" stroke-width="3"></circle>
                <path class="opacity-75" fill="currentColor" d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4zm2 5.291A7.962 7.962 0 014 12H0c0 3.042 1.135 5.824 3 7.938l3-2.647z"></path>
              </svg>
            {/if}
          </span>
          <input
            type="text"
            bind:this={input}
            bind:value={searchQuery}
            oninput={onQueryInput}
            placeholder={scope === 'page' ? 'Search this page…' : 'Search resources, paths, everything (type new for create)...'}
            class="command-palette-input"
            autocomplete="off"
            spellcheck="false"
          />
          {#if pageItems.length > 0}
            <div class="command-palette-scope" role="group" aria-label="Search scope" title="Press Tab to switch between global search and this page">
              <button type="button" onclick={() => setScope('global')} aria-pressed={scope === 'global'} class="command-palette-scope-option">Global</button>
              <button type="button" onclick={() => setScope('page')} aria-pressed={scope === 'page'} class="command-palette-scope-option">This page</button>
            </div>
          {/if}
        </div>

        {#if searchQuery.length >= 1 || scope === 'page' || selecting}
          <div class="command-palette-body relative">
            {#if transitioning}
              <div class="absolute inset-0 z-30 flex items-center justify-center bg-white/50 backdrop-blur-[2px] dark:bg-black/40">Loading…</div>
            {/if}
            {#if selecting}
              <div class="relative min-h-32">
                <div class="command-palette-section transition-all">
                  <div>
                    <div class="command-palette-step-header">
                      <button type="button" onclick={goBack} class="command-palette-step-back" title="Back">
                        <Reicon name="arrow-right" class="size-3.5 rotate-180" />
                      </button>
                      <div class="min-w-0">
                        <div class="command-palette-step-title">{stepTitle}</div>
                        {#if selectedName}
                          <div class="command-palette-step-subtitle">for {selectedName}</div>
                        {/if}
                      </div>
                    </div>
                    {#if stepItems.length > 0}
                      {#each stepItems as item}
                        <button type="button" onclick={() => pickStep(item)} class="search-result-item command-palette-item">
                          <div class="command-palette-item-main">
                            <div class="command-palette-item-name">{item.name}</div>
                            {#if selectedServer !== null && selectedDestination === null}
                              <div class="command-palette-item-meta">Network: {item.network ?? 'default'}</div>
                            {:else if item.description}
                              <div class="command-palette-item-meta">{item.description}</div>
                            {/if}
                          </div>
                          <Reicon name="arrow-right" class="command-palette-item-chevron" />
                        </button>
                      {/each}
                    {:else}
                      <div class="command-palette-status is-error">{stepEmpty}</div>
                    {/if}
                  </div>
                </div>
              </div>
            {/if}

            {#if pageResults.length > 0 && !selecting}
              <div class="command-palette-section">
                <div class="command-palette-group-label">This page</div>
                {#each pageResults as item (item.href)}
                  <a
                    href={item.element ? (item.element.id ? `#${item.element.id}` : '#') : item.href}
                    onclick={(e) => {
                      e.preventDefault();
                      openPageItem(item);
                    }}
                    class="search-result-item command-palette-item"
                  >
                    <div class="command-palette-item-main">
                      <div class="command-palette-item-title"><span class="command-palette-item-name">{item.label}</span></div>
                      <div class="command-palette-item-meta">{item.breadcrumb}</div>
                    </div>
                    <svg class="command-palette-item-chevron" viewBox="0 0 24 24" fill="none" aria-hidden="true">
                      <path d="M9 5l7 7-7 7" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" />
                    </svg>
                  </a>
                {/each}
              </div>
            {/if}

            {#if searchQuery.length >= 1 && searchResults.length > 0 && !selecting}
              <div class="command-palette-section">
                {#if filteredCreatable.length > 0 || pageResults.length > 0}
                  <div class="command-palette-group-label">Existing resources</div>
                {/if}
                {#each searchResults as result}
                  <a
                    href={result.link || '#'}
                    onclick={(e) => {
                      if (e.metaKey || e.ctrlKey || e.shiftKey) return;
                      e.preventDefault();
                      closeModal();
                      router.go(result.link || '/');
                    }}
                    class="search-result-item command-palette-item"
                  >
                    <div class="command-palette-item-main">
                      <div class="command-palette-item-title">
                        <span class="command-palette-item-name">{result.name}</span>
                        <span class="command-palette-type-badge"><span>{typeLabel(result)}</span></span>
                      </div>
                      {#if result.project && result.environment}
                        <div class="command-palette-item-meta"><span>{result.project}</span> / <span>{result.environment}</span></div>
                      {/if}
                      {#if result.description}
                        <div class="command-palette-item-meta">{result.description.length > 80 ? `${result.description.substring(0, 80)}...` : result.description}</div>
                      {/if}
                    </div>
                    <svg class="command-palette-item-chevron" viewBox="0 0 24 24" fill="none" aria-hidden="true">
                      <path d="M9 5l7 7-7 7" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" />
                    </svg>
                  </a>
                {/each}
              </div>
            {/if}

            {#if filteredCreatable.length > 0 && !selecting}
              <div>
                {#each grouped as [categoryName, items] (categoryName)}
                  <div class="command-palette-section">
                    <div class="command-palette-group-label">{categoryName}</div>
                    {#each items as item (item.type)}
                      <button type="button" onclick={() => navigateToResource(item.type)} class="search-result-item command-palette-item">
                        {#if item.logo}
                          <div class="command-palette-item-icon">
                            <img
                              src={item.logo.startsWith('http') ? item.logo : `/${item.logo}`}
                              alt={item.name}
                              onerror={(e) => {
                                const el = e.currentTarget as HTMLImageElement;
                                if (item.logo_default_url && !el.dataset.defaultTried) {
                                  el.dataset.defaultTried = 'true';
                                  el.src = `/${item.logo_default_url}`;
                                }
                              }}
                            />
                          </div>
                        {:else}
                          <div class="command-palette-item-icon is-create">
                            <svg class="size-3.5" viewBox="0 0 24 24" fill="none" aria-hidden="true">
                              <path d="M6 12H18" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" />
                              <path d="M12 18V6" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" />
                            </svg>
                          </div>
                        {/if}
                        <div class="command-palette-item-main">
                          <div class="command-palette-item-title">
                            <span class="command-palette-item-name">{item.name}</span>
                            {#if item.amd_only}
                              <span class="command-palette-arch-badge" title="This service only supports AMD64/x86_64 architecture">AMD only</span>
                            {/if}
                            {#if item.arm_only}
                              <span class="command-palette-arch-badge" title="This service only supports ARM64/aarch64 architecture">ARM only</span>
                            {/if}
                            {#if item.quickcommand}
                              <span class="command-palette-quickcommand">{item.quickcommand.replace(/^\(type:\s*/i, '').replace(/\)$/, '')}</span>
                            {/if}
                          </div>
                          <div class="command-palette-item-meta">{item.description}</div>
                        </div>
                        <svg class="command-palette-item-chevron" viewBox="0 0 24 24" fill="none" aria-hidden="true">
                          <path d="M9 5l7 7-7 7" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" />
                        </svg>
                      </button>
                    {/each}
                  </div>
                {/each}
              </div>
            {/if}

            {#if scope === 'global' && searchQuery.length >= 2 && pageResults.length === 0 && searchResults.length === 0 && filteredCreatable.length === 0 && !selecting && !loading}
              <div class="command-palette-empty">
                <p class="command-palette-empty-title">No results found</p>
                <p class="command-palette-empty-desc">
                  Try different keywords, or type <span class="font-medium">new</span> to create a resource.
                </p>
              </div>
            {/if}

            {#if scope === 'page' && pageResults.length === 0}
              <div class="command-palette-empty">
                <p class="command-palette-empty-title">Nothing on this page</p>
                <p class="command-palette-empty-desc">Press <span class="font-medium">Tab</span> to search everything.</p>
              </div>
            {/if}
          </div>
        {/if}
      </div>
    {/if}
  </div>
</div>

{#if createModal === 'project'}
  <NewProjectModal title="New project" onclose={() => (createModal = null)} />
{:else if createModal === 'team'}
  <NewTeamModal title="New team" onclose={() => (createModal = null)} />
{/if}
