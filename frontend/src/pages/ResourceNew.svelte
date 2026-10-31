<script lang="ts">
  // Port of upstream livewire/project/resource/create.blade.php with the
  // picker livewire/project/new/select.blade.php (Project\New\Select).
  // Only PostgreSQL can be created by coolify-rs; every other resource is
  // listed as upstream lists it and says it is not available yet. Service
  // templates are not loaded, and the deploy canvas is left out (ADR 0008).
  import NewDockerImage from '../components/NewDockerImage.svelte';
  import NewPublicRepository from '../components/NewPublicRepository.svelte';
  import NewPrivateRepository from '../components/NewPrivateRepository.svelte';
  import NewDeployKeyRepository from '../components/NewDeployKeyRepository.svelte';
  import NewSimpleDockerfile from '../components/NewSimpleDockerfile.svelte';
  import NewDockerCompose from '../components/NewDockerCompose.svelte';
  import Reicon from '../components/Reicon.svelte';
  import Empty from '../components/Empty.svelte';
  import Callout from '../components/Callout.svelte';
  import Dropdown from '../components/Dropdown.svelte';
  import SettingsSection from '../components/SettingsSection.svelte';
  import NewResourceCard from '../components/NewResourceCard.svelte';
  import { dbUi, uiApi, ApiError, type DestinationServer, type ProjectPageData } from '../lib/api';
  import { link, router } from '../lib/router.svelte';
  import { setProjectCrumbs } from '../lib/crumbs';
  import { notify } from '../lib/toast.svelte';

  let { uuid, env }: { uuid: string; env: string } = $props();

  interface Resource {
    id: string;
    name: string;
    description: string;
    logo: string;
    logoDark?: string;
    documentation?: string;
    experimental?: boolean;
  }

  const logo = (name: string) => `/svgs/resources/${name}.svg`;

  // Select::loadServices()
  const gitBasedApplications: Resource[] = [
    {
      id: 'public',
      name: 'Public Git Repository',
      description: 'Deploy any public Git repository. coolify-rs builds it from source, no credentials required.',
      documentation: 'https://coolify.io/docs/applications/ci-cd',
      logo: logo('public-repo'),
    },
    {
      id: 'private-deploy-key',
      name: 'Private Git Repository (with Deploy Key)',
      description:
        'Deploy a private repository over SSH with a repository-scoped deploy key. Set the URL and branch manually, no automatic deploys.',
      documentation: 'https://coolify.io/docs/applications/ci-cd/github/deploy-key',
      logo: logo('deploy-key'),
    },
    {
      id: 'private-gh-app',
      name: 'Git Repository (with GitHub App)',
      description: 'Deploy public or private GitHub repositories through a GitHub App, with automatic webhooks and pull request previews.',
      documentation: 'https://coolify.io/docs/applications/ci-cd/github/setup-app',
      logo: logo('github-app'),
      logoDark: logo('github-app-dark'),
    },
    {
      id: 'private-gitlab-app',
      name: 'Git Repository (with GitLab App)',
      description: 'Deploy public or private GitLab projects through a GitLab App, with automatic webhooks and merge request previews.',
      logo: logo('gitlab-app'),
    },
    {
      id: 'private-gitea-app',
      name: 'Git Repository (with Gitea App)',
      description: 'Deploy public or private Gitea repositories through a Gitea App, with automatic webhooks and pull request previews.',
      logo: '/svgs/gitea.svg',
    },
  ];
  const dockerBasedApplications: Resource[] = [
    {
      id: 'dockerfile',
      name: 'Dockerfile',
      description: 'Deploy an application using a Dockerfile, without a Git repository.',
      documentation: 'https://coolify.io/docs/applications/build-packs/dockerfile',
      logo: logo('dockerfile'),
    },
    {
      id: 'docker-compose-empty',
      name: 'Docker Compose',
      description: 'Deploy a multi-container application using a Docker Compose file, without a Git repository.',
      documentation: 'https://coolify.io/docs/applications/build-packs/docker-compose',
      logo: logo('docker-compose'),
    },
    {
      id: 'docker-image',
      name: 'Docker Image',
      description: 'Deploy an application using a prebuilt image from any Docker registry, without a Git repository.',
      documentation: 'https://coolify.io/docs/applications',
      logo: logo('docker-image'),
    },
  ];
  const databases: Resource[] = [
    { id: 'postgresql', name: 'PostgreSQL', description: 'A relational database with strong SQL standards support and extensibility.', logo: logo('postgres') },
    { id: 'mysql', name: 'MySQL', description: 'A relational database for web and general-purpose applications.', logo: logo('mysql') },
    { id: 'mariadb', name: 'MariaDB', description: 'A relational database and drop-in replacement for MySQL.', logo: logo('mariadb') },
    { id: 'redis', name: 'Redis', description: 'An in-memory key-value store used as a database, cache, and message broker.', logo: logo('redis') },
    { id: 'keydb', name: 'KeyDB', description: 'A multithreaded, Redis-compatible in-memory store.', logo: logo('keydb'), logoDark: logo('keydb-dark') },
    {
      id: 'dragonfly',
      name: 'Dragonfly',
      description: 'An in-memory datastore compatible with Redis and Memcached.',
      logo: logo('dragonfly'),
      logoDark: logo('dragonfly-dark'),
    },
    { id: 'mongodb', name: 'MongoDB', description: 'A document-oriented NoSQL database that stores JSON-like documents.', logo: logo('mongodb') },
    { id: 'clickhouse', name: 'ClickHouse', description: 'A column-oriented database for real-time analytics over large datasets.', logo: logo('clickhouse') },
    { id: 'sqlite', name: 'SQLite', description: 'A lightweight relational database stored in a single file.', logo: logo('sqlite'), experimental: true },
  ];
  const databaseLinks: Record<string, { docs: string; website: string }> = {
    postgresql: { docs: 'https://www.postgresql.org/docs/', website: 'https://www.postgresql.org/' },
    mysql: { docs: 'https://dev.mysql.com/doc/', website: 'https://www.mysql.com/' },
    mariadb: { docs: 'https://mariadb.com/kb/en/documentation/', website: 'https://mariadb.org/' },
    redis: { docs: 'https://redis.io/docs/latest/', website: 'https://redis.io/' },
    keydb: { docs: 'https://docs.keydb.dev/', website: 'https://keydb.dev/' },
    dragonfly: { docs: 'https://www.dragonflydb.io/docs', website: 'https://www.dragonflydb.io/' },
    mongodb: { docs: 'https://www.mongodb.com/docs/', website: 'https://www.mongodb.com/' },
    clickhouse: { docs: 'https://clickhouse.com/docs', website: 'https://clickhouse.com/' },
    sqlite: { docs: 'https://sqlite.org/docs.html', website: 'https://sqlite.org/' },
  };

  // The `select-postgresql-type` step.
  const postgresImages: { image: string; name: string; badge?: 'default' | 'amd'; description: string; docs: string }[] = [
    {
      image: 'postgres:18-alpine',
      name: 'PostgreSQL 18',
      badge: 'default',
      description: 'PostgreSQL is a powerful, open-source object-relational database system (no extensions).',
      docs: 'https://hub.docker.com/_/postgres/',
    },
    {
      image: 'postgres:17-alpine',
      name: 'PostgreSQL 17',
      description: 'PostgreSQL is a powerful, open-source object-relational database system (no extensions).',
      docs: 'https://hub.docker.com/_/postgres/',
    },
    {
      image: 'postgres:16-alpine',
      name: 'PostgreSQL 16',
      description: 'PostgreSQL is a powerful, open-source object-relational database system (no extensions).',
      docs: 'https://hub.docker.com/_/postgres/',
    },
    {
      image: 'supabase/postgres:17.4.1.032',
      name: 'Supabase PostgreSQL',
      description: 'Supabase is a modern, open-source alternative to PostgreSQL with lots of extensions.',
      docs: 'https://github.com/supabase/postgres',
    },
    {
      image: 'postgis/postgis:17-3.5-alpine',
      name: 'PostGIS',
      badge: 'amd',
      description: 'PostGIS is a PostgreSQL extension for geographic objects.',
      docs: 'https://github.com/postgis/docker-postgis',
    },
    {
      image: 'pgvector/pgvector:pg18',
      name: 'PGVector 18',
      description: 'PGVector is a PostgreSQL extension for vector data types.',
      docs: 'https://github.com/pgvector/pgvector',
    },
    {
      image: 'pgvector/pgvector:pg17',
      name: 'PGVector 17',
      description: 'PGVector is a PostgreSQL extension for vector data types.',
      docs: 'https://github.com/pgvector/pgvector',
    },
  ];

  const resourceTypeOptions = [
    { value: 'all', label: 'All resources' },
    { value: 'applications', label: 'Applications' },
    { value: 'databases', label: 'Databases' },
    { value: 'services', label: 'Services' },
  ];

  let project = $state<ProjectPageData | null>(null);
  let missing = $state(false);

  let step = $state<'type' | 'servers' | 'destinations' | 'select-postgresql-type'>('type');
  let selectedType = $state('postgresql');
  // Database types coolify-rs can create (SQLite is not ported).
  const portedDatabases = ['postgresql', 'mysql', 'mariadb', 'redis', 'keydb', 'dragonfly', 'mongodb', 'clickhouse'];
  // Application types coolify-rs can create.
  const portedApplications = ['docker-image', 'public', 'private-gh-app', 'private-gitlab-app', 'private-gitea-app', 'private-deploy-key', 'dockerfile', 'docker-compose-empty'];
  // `Create::mount()`: `?type=…&destination=…` renders that type's form.
  const formType = $derived(router.query('type'));
  const formDestination = $derived(router.query('destination') ?? '');
  let search = $state('');
  let resourceType = $state('all');
  let searchInput: HTMLInputElement | undefined = $state();
  let selecting = $state(false);
  let categoryOpen = $state(false);
  let categorySearch = $state('');
  let categoryRoot: HTMLDivElement | undefined = $state();
  let categorySearchInput: HTMLInputElement | undefined = $state();

  let allServers = $state<DestinationServer[] | null>(null);
  let loadingServers = $state(false);
  let serversPromise: Promise<void> | null = null;
  let server = $state<DestinationServer | null>(null);
  let destinationUuid = $state('');
  let creating = $state(false);

  // Select::loadServers(): usable servers, and build servers that cannot host resources.
  const servers = $derived((allServers ?? []).filter((s) => s.functional && !s.is_build));
  const buildServers = $derived((allServers ?? []).filter((s) => s.functional && s.is_build));
  const onlyBuildServerAvailable = $derived(servers.length === 0 && buildServers.length > 0);

  $effect(() => {
    uiApi
      .project(uuid)
      .then((d) => {
        if (!d.environments.some((x) => x.uuid === env)) {
          missing = true;
          return;
        }
        project = d;
        setProjectCrumbs(d, env);
        const name = d.project.name;
        document.title = `${name.length > 10 ? name.slice(0, 10) + '...' : name} > New | coolify-rs`;
      })
      .catch((e) => {
        if (e instanceof ApiError && e.status === 404) missing = true;
        else throw e;
      });
    loadServers();
  });

  function loadServers(): Promise<void> {
    loadingServers = true;
    serversPromise = dbUi
      .destinations()
      .then((r) => {
        allServers = r.servers.slice().sort((a, b) => a.name.localeCompare(b.name));
      })
      .catch((e) => {
        allServers = [];
        notify.error(e instanceof ApiError ? e.message : String(e));
      })
      .finally(() => (loadingServers = false));
    return serversPromise;
  }

  $effect(() => {
    if (step === 'type' && project) searchInput?.focus();
  });

  function matches(r: Resource): boolean {
    const q = search.trim().toLowerCase();
    return q === '' || r.name.toLowerCase().includes(q) || r.description.toLowerCase().includes(q);
  }
  const filteredGit = $derived(gitBasedApplications.filter(matches));
  const filteredDocker = $derived(dockerBasedApplications.filter(matches));
  const filteredDatabases = $derived(databases.filter(matches));
  const showApplications = $derived(
    (resourceType === 'all' || resourceType === 'applications') && filteredGit.length + filteredDocker.length > 0,
  );
  const showDatabases = $derived((resourceType === 'all' || resourceType === 'databases') && filteredDatabases.length > 0);
  // Service templates are not loaded, so the section only says they are not available.
  const showServices = $derived((resourceType === 'all' || resourceType === 'services') && search.trim() === '');

  async function setType(type: string) {
    if (selecting) return;
    if (!portedDatabases.includes(type) && !portedApplications.includes(type)) {
      notify.info('Not available in coolify-rs yet', 'This resource type cannot be created in coolify-rs yet. Use Coolify for this resource for now.');
      return;
    }
    selectedType = type;
    selecting = true;
    try {
      if (serversPromise) await serversPromise;
      if (servers.length === 1 && buildServers.length === 0) {
        setServer(servers[0]);
        return;
      }
      step = 'servers';
    } finally {
      selecting = false;
    }
  }

  function setServer(s: DestinationServer) {
    server = s;
    if (s.destinations.length === 1) {
      setDestination(s.destinations[0].uuid);
      return;
    }
    step = 'destinations';
  }

  function setDestination(destination: string) {
    destinationUuid = destination;
    // `whatToDoNext()`: application types continue on their own form.
    if (portedApplications.includes(selectedType)) {
      const serverId = server?.id ?? '';
      router.go(`/project/${uuid}/environment/${env}/new?type=${selectedType}&destination=${destination}&server_id=${serverId}`);
      return;
    }
    if (selectedType === 'postgresql') {
      step = 'select-postgresql-type';
      return;
    }
    // Other databases are created right away (`resource.create` without an image).
    createDatabase(selectedType);
  }

  function setPostgresqlType(image: string) {
    createDatabase('postgresql', image);
  }

  async function createDatabase(type: string, image?: string) {
    if (creating) return;
    creating = true;
    try {
      const { uuid: dbUuid } = await dbUi.create({
        project_uuid: uuid,
        environment_uuid: env,
        destination_uuid: destinationUuid,
        type,
        ...(image ? { database_image: image } : {}),
      });
      router.go(`/project/${uuid}/environment/${env}/database/${dbUuid}`);
    } catch (e) {
      notify.error(e instanceof ApiError ? e.message : String(e));
      creating = false;
    }
  }

  function reload() {
    loadServers();
  }

  const row =
    'group flex min-h-14 w-full items-center gap-3 px-4 py-3 text-left transition-colors hover:bg-neutral-50 dark:hover:bg-white/[0.025]';
  const rowIcon =
    'flex size-8 shrink-0 items-center justify-center rounded-lg border border-neutral-200 bg-neutral-50 text-neutral-500 dark:border-white/[0.08] dark:bg-white/[0.035] dark:text-fg-dim';
  const badge =
    'inline-flex h-6 max-w-full items-center gap-1.5 whitespace-nowrap rounded-full border border-neutral-200 bg-neutral-100 px-2 text-xs font-medium leading-none text-neutral-700 dark:border-white/[0.12] dark:bg-white/[0.07] dark:text-white';
</script>

<svelte:window
  onclick={(e) => {
    if (categoryOpen && categoryRoot && !categoryRoot.contains(e.target as Node)) {
      categoryOpen = false;
      categorySearch = '';
    }
  }}
  onkeydown={(e) => {
    if (step === 'type' && e.key === '/' && searchInput) {
      e.preventDefault();
      searchInput.focus();
    }
  }}
/>

{#snippet statusBadge(text: string)}
  <!-- components/status-badge.blade.php, neutral dot -->
  <span class={badge}>
    <span class="relative flex size-1.5 shrink-0">
      <span class="relative inline-flex size-1.5 rounded-full bg-neutral-400 dark:bg-neutral-500"></span>
    </span>
    <span class="truncate">{text}</span>
  </span>
{/snippet}

{#if missing}
  <div class="application-settings-form w-full">
    <Empty title="Environment not found" description="It may have been deleted, or it belongs to another team." iconName="layers" size="lg" />
  </div>
{:else if project && formType === 'docker-image' && formDestination}
  <div>
    <NewDockerImage project={uuid} {env} destination={formDestination} />
  </div>
{:else if project && formType === 'public' && formDestination}
  <div>
    <NewPublicRepository project={uuid} {env} destination={formDestination} />
  </div>
{:else if project && formType === 'private-gh-app' && formDestination}
  <div>
    <NewPrivateRepository provider="github" project={uuid} {env} destination={formDestination} />
  </div>
{:else if project && formType === 'private-gitlab-app' && formDestination}
  <div>
    <NewPrivateRepository provider="gitlab" project={uuid} {env} destination={formDestination} />
  </div>
{:else if project && formType === 'private-gitea-app' && formDestination}
  <div>
    <NewPrivateRepository provider="gitea" project={uuid} {env} destination={formDestination} />
  </div>
{:else if project && formType === 'private-deploy-key' && formDestination}
  <div>
    <NewDeployKeyRepository project={uuid} {env} destination={formDestination} />
  </div>
{:else if project && formType === 'dockerfile' && formDestination}
  <div>
    <NewSimpleDockerfile project={uuid} {env} destination={formDestination} />
  </div>
{:else if project && formType === 'docker-compose-empty' && formDestination}
  <div>
    <NewDockerCompose project={uuid} {env} destination={formDestination} />
  </div>
{:else if project}
  <div>
    <div class="application-settings-form">
      {#if step === 'type'}
        <div>
          <SettingsSection title="Choose a resource" flush>
            {#snippet actions()}
              <button type="button" class="button" disabled={loadingServers} onclick={reload}>
                <Reicon name="refresh" class="size-3.5" />
                Reload
              </button>
            {/snippet}
            <div
              class="flex flex-col gap-3 border-b border-neutral-200 p-4 dark:border-white/[0.08] sm:flex-row sm:items-center sm:justify-between"
            >
              <div class="relative min-w-0 flex-1">
                <Reicon
                  name="search"
                  class="pointer-events-none absolute top-1/2 left-2.5 z-10 size-3.5 -translate-y-1/2 text-neutral-400 dark:text-fg-faint"
                />
                <input
                  autocomplete="off"
                  bind:this={searchInput}
                  bind:value={search}
                  type="search"
                  placeholder="Search resources"
                  class="h-8! w-full rounded-lg! border-neutral-200! bg-white! py-0! pr-8! pl-8! text-[12px]! shadow-none! placeholder:text-neutral-400 focus:border-accent! focus:ring-0! dark:border-white/[0.08]! dark:bg-white/[0.035]! dark:text-fg! dark:placeholder:text-fg-faint"
                />
              </div>

              <div class="flex shrink-0 items-center gap-2">
                <Dropdown panelClass="w-48!">
                  {#snippet trigger({ open })}
                    <button type="button" class="button" aria-haspopup="listbox" aria-expanded={open}>
                      <Reicon name="filter" class="size-3.5" />
                      Filter
                    </button>
                  {/snippet}
                  {#snippet children({ close })}
                    <div class="px-2 py-1 text-[10px] font-semibold tracking-wide text-neutral-400 uppercase dark:text-fg-faint">
                      Resource type
                    </div>
                    {#each resourceTypeOptions as option (option.value)}
                      <button
                        type="button"
                        class="listbox-option"
                        role="option"
                        aria-selected={resourceType === option.value}
                        onclick={() => {
                          resourceType = option.value;
                          close();
                        }}
                      >
                        <span>{option.label}</span>
                        {#if resourceType === option.value}
                          <Reicon name="check" class="size-3.5 text-black dark:text-fg" />
                        {/if}
                      </button>
                    {/each}
                  {/snippet}
                </Dropdown>

                <!-- Service categories come from the templates, which coolify-rs does not load: only "All categories". -->
                <div class="relative w-48" bind:this={categoryRoot}>
                  <button
                    type="button"
                    class="listbox-trigger"
                    onclick={() => {
                      categoryOpen = !categoryOpen;
                      if (categoryOpen) requestAnimationFrame(() => categorySearchInput?.focus());
                    }}
                    aria-haspopup="listbox"
                    aria-controls="resource-category-options"
                    aria-expanded={categoryOpen}
                    title="All categories"
                  >
                    <span class="listbox-trigger-label capitalize">All categories</span>
                    <svg class="size-3.5 shrink-0 opacity-60" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                      <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="m8 9 4-4 4 4m0 6-4 4-4-4" />
                    </svg>
                  </button>
                  {#if categoryOpen}
                    <div
                      id="resource-category-options"
                      role="listbox"
                      tabindex="-1"
                      aria-label="Service category"
                      onkeydown={(e) => e.key === 'Escape' && (e.stopPropagation(), (categoryOpen = false))}
                      class="listbox-panel left-auto! right-0! z-[90]! min-w-56!"
                    >
                      <div class="border-b border-neutral-200 p-2 dark:border-white/[0.08]">
                        <input
                          type="search"
                          bind:this={categorySearchInput}
                          bind:value={categorySearch}
                          placeholder="Search categories"
                          class="h-8! w-full rounded-md! border-neutral-200! bg-neutral-50! px-2.5! py-0! text-[12px]! shadow-none! focus:ring-0! dark:border-white/[0.08]! dark:bg-white/[0.04]! dark:text-fg!"
                        />
                      </div>
                      <div class="max-h-60 overflow-auto p-1">
                        <button
                          type="button"
                          class="listbox-option"
                          role="option"
                          aria-selected="true"
                          onclick={() => {
                            categorySearch = '';
                            categoryOpen = false;
                          }}
                        >
                          <span>All categories</span>
                          <Reicon name="check" class="size-3.5 text-black dark:text-fg" />
                        </button>
                      </div>
                    </div>
                  {/if}
                </div>
              </div>
            </div>
          </SettingsSection>

          <div class="mt-6 flex flex-col gap-6">
            {#if showApplications}
              <section class="application-settings-section">
                <div class="application-settings-section-header">
                  <div class="flex items-center gap-2">
                    <Reicon name="globe" class="size-4 text-neutral-400 dark:text-fg-faint" />
                    <h2>Applications</h2>
                  </div>
                </div>
                <div class="application-settings-section-body grid grid-cols-1 justify-start gap-3 text-left md:grid-cols-2 xl:grid-cols-3">
                  {#each filteredGit as app (app.name)}
                    <NewResourceCard
                      name={app.name}
                      description={app.description}
                      logo={app.logo}
                      logoDark={app.logoDark}
                      source="Git source"
                      docs={app.documentation}
                      onselect={() => setType(app.id)}
                    />
                  {/each}
                  {#each filteredDocker as app (app.name)}
                    <NewResourceCard
                      name={app.name}
                      description={app.description}
                      logo={app.logo}
                      source="Docker source"
                      docs={app.documentation}
                      onselect={() => setType(app.id)}
                    />
                  {/each}
                </div>
              </section>
            {/if}

            {#if showDatabases}
              <section class="application-settings-section">
                <div class="application-settings-section-header">
                  <div class="flex items-center gap-2">
                    <Reicon name="database" class="size-4 text-neutral-400 dark:text-fg-faint" />
                    <h2>Databases</h2>
                  </div>
                </div>
                <div class="application-settings-section-body grid grid-cols-1 justify-start gap-3 text-left md:grid-cols-2 xl:grid-cols-3">
                  {#each filteredDatabases as database (database.id)}
                    <NewResourceCard
                      name={database.name}
                      description={database.description}
                      logo={database.logo}
                      logoDark={database.logoDark}
                      experimental={database.experimental}
                      docs={databaseLinks[database.id]?.docs}
                      website={databaseLinks[database.id]?.website}
                      onselect={() => setType(database.id)}
                    />
                  {/each}
                </div>
              </section>
            {/if}

            {#if showServices}
              <section class="application-settings-section">
                <div class="application-settings-section-header">
                  <div class="flex items-center gap-2">
                    <Reicon name="layers" class="size-4 text-neutral-400 dark:text-fg-faint" />
                    <h2>Services</h2>
                  </div>
                </div>
                <div class="application-settings-section-body">
                  <Callout type="info" title="Not available in coolify-rs yet">
                    One-click service templates have not been ported to coolify-rs. Use Coolify for them for now.
                  </Callout>
                </div>
              </section>
            {/if}

            {#if !showApplications && !showDatabases && !showServices}
              <div>
                <Empty title="No resources found" description="Try a different search or resource type." iconName="layers" size="sm" />
              </div>
            {/if}
          </div>
        </div>
      {/if}

      {#if step === 'servers'}
        <SettingsSection title="Select a server" description="Choose the machine that will host this resource." flush>
          {#if onlyBuildServerAvailable}
            <Callout type="warning" title="No deployment server" class="m-4">
              Only build servers are available. Add or reconfigure a server before continuing.
              <a class="font-medium underline" href="/servers" use:link>Open servers</a>
            </Callout>
          {/if}
          <div class="divide-y divide-neutral-200 dark:divide-white/[0.07]">
            {#each servers as s (s.uuid)}
              <button type="button" onclick={() => setServer(s)} class={row}>
                <span class={rowIcon}><Reicon name="servers" class="size-4" /></span>
                <span class="min-w-0 flex-1">
                  <span class="block truncate text-[13px] font-semibold text-black dark:text-fg">{s.name}</span>
                  <span class="block truncate text-[11px] text-neutral-500 dark:text-fg-faint">{s.description || s.ip}</span>
                </span>
                {@render statusBadge('running')}
              </button>
            {:else}
              {#if buildServers.length === 0}
                <Empty
                  title="No available servers"
                  description="Validate a reachable server before creating this resource."
                  iconName="servers"
                  size="sm"
                >
                  {#snippet contents()}
                    <a class="button" href="/servers" use:link>Open servers</a>
                  {/snippet}
                </Empty>
              {/if}
            {/each}

            {#each buildServers as s (s.uuid)}
              <div class="flex min-h-14 items-center gap-3 px-4 py-3 opacity-55">
                <span class={rowIcon}><Reicon name="servers" class="size-4" /></span>
                <span class="min-w-0 flex-1">
                  <span class="block truncate text-[13px] font-semibold text-black dark:text-fg">{s.name}</span>
                  <span class="block text-[11px] text-neutral-500 dark:text-fg-faint">Build-only servers cannot host resources.</span>
                </span>
                {@render statusBadge('exited')}
                <a href="/server/{s.uuid}" use:link class="button">Settings</a>
              </div>
            {/each}
          </div>
        </SettingsSection>
      {/if}

      {#if step === 'destinations' && server}
        <SettingsSection
          title="Select a destination"
          description="Destinations separate resources by Docker network. Use the default destination when unsure."
          flush
        >
          <div class="divide-y divide-neutral-200 dark:divide-white/[0.07]">
            {#each server.destinations as d (d.uuid)}
              <button type="button" onclick={() => setDestination(d.uuid)} class={row}>
                <span class={rowIcon}><Reicon name="destinations" class="size-4" /></span>
                <span class="min-w-0 flex-1">
                  <span class="block truncate text-[13px] font-semibold text-black dark:text-fg">{d.name}</span>
                  <span class="block truncate text-[11px] text-neutral-500 dark:text-fg-faint">Network: {d.network}</span>
                </span>
                {@render statusBadge('running')}
              </button>
            {/each}
          </div>
        </SettingsSection>
      {/if}

      {#if step === 'select-postgresql-type'}
        <div>
          <div class="mb-4">
            <h2 class="text-[15px]! font-semibold!">Select a PostgreSQL image</h2>
            <p class="mt-1 text-[12px] text-neutral-500 dark:text-fg-dim">Use PostgreSQL 18 unless the workload needs bundled extensions.</p>
          </div>
          <div class="grid grid-cols-1 gap-3 lg:grid-cols-2">
            {#each postgresImages as pg (pg.image)}
              <div
                class="group relative flex min-h-24 items-center gap-3 rounded-[10px] border border-neutral-200 bg-white p-4 transition-colors hover:border-neutral-300 hover:bg-neutral-50 dark:border-white/[0.07] dark:bg-surface dark:hover:border-white/[0.12] dark:hover:bg-white/[0.035] {creating
                  ? 'cursor-not-allowed opacity-50'
                  : 'cursor-pointer'}"
                role="button"
                tabindex="-1"
                aria-disabled={creating}
                onclick={() => setPostgresqlType(pg.image)}
                onkeydown={(e) => e.key === 'Enter' && setPostgresqlType(pg.image)}
              >
                <div class="flex flex-col">
                  <div class="text-[13px] font-semibold text-black dark:text-fg">
                    {pg.name}
                    {#if pg.badge === 'default'}
                      <span
                        class="ml-1 rounded-full bg-coollabs/10 px-2 py-0.5 text-[10px] font-medium text-coollabs dark:bg-brand/15 dark:text-brand-light"
                        >Default</span
                      >
                    {:else if pg.badge === 'amd'}
                      <span class="ml-1 text-[10px] font-medium text-amber-600 dark:text-amber-300">AMD only</span>
                    {/if}
                  </div>
                  <div class="mt-1 pr-8 text-[11px] leading-4 text-neutral-500 dark:text-fg-faint">{pg.description}</div>
                </div>
                <a
                  href={pg.docs}
                  target="_blank"
                  onclick={(e) => e.stopPropagation()}
                  class="absolute top-2 right-2 flex size-7 items-center justify-center rounded-md text-neutral-400 transition-colors hover:bg-neutral-100 hover:text-black dark:text-fg-faint dark:hover:bg-white/[0.06] dark:hover:text-fg"
                  title="View documentation"
                >
                  <svg class="w-4 h-4 text-neutral-600 dark:text-neutral-400" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                    <path
                      stroke-linecap="round"
                      stroke-linejoin="round"
                      stroke-width="2"
                      d="M9 12h6m-6 4h6m2 5H7a2 2 0 01-2-2V5a2 2 0 012-2h5.586a1 1 0 01.707.293l5.414 5.414a1 1 0 01.293.707V19a2 2 0 01-2 2z"
                    />
                  </svg>
                </a>
              </div>
            {/each}
          </div>
        </div>
      {/if}
    </div>
  </div>
{/if}
