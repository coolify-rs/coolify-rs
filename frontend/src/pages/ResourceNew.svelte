<script lang="ts">
  // Port of upstream livewire/project/resource/create.blade.php (Project\Resource\Create) with the
  // picker livewire/project/new/select.blade.php (Project\New\Select). A resource type coolify-rs
  // cannot create yet is listed as upstream lists it and says so when it is chosen.
  import NewDockerImage from '../components/NewDockerImage.svelte';
  import NewPublicRepository from '../components/NewPublicRepository.svelte';
  import NewPrivateRepository from '../components/NewPrivateRepository.svelte';
  import NewDeployKeyRepository from '../components/NewDeployKeyRepository.svelte';
  import NewSimpleDockerfile from '../components/NewSimpleDockerfile.svelte';
  import NewDockerCompose from '../components/NewDockerCompose.svelte';
  import Reicon from '../components/Reicon.svelte';
  import BetaBadge from '../components/BetaBadge.svelte';
  import Empty from '../components/Empty.svelte';
  import Callout from '../components/Callout.svelte';
  import { servicesApi, type ServiceTemplate } from '../lib/servicesApi';
  import Dropdown from '../components/Dropdown.svelte';
  import SettingsSection from '../components/SettingsSection.svelte';
  import NewResourceCard, { newResourceViewClasses } from '../components/NewResourceCard.svelte';
  import StatusBadge from '../components/StatusBadge.svelte';
  import Loading from '../components/Loading.svelte';
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
      name: 'Git Repository (with Gitea / Forgejo App)',
      description:
        'Deploy public or private Gitea, Forgejo, or Codeberg repositories through a Gitea / Forgejo App, with automatic webhooks and pull request previews.',
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
    { id: 'postgresql', name: 'PostgreSQL', description: 'An extensible relational database with strong SQL support.', logo: logo('postgres') },
    { id: 'mysql', name: 'MySQL', description: 'A relational database for web and general-purpose applications.', logo: logo('mysql') },
    { id: 'mariadb', name: 'MariaDB', description: 'A relational database and drop-in replacement for MySQL.', logo: logo('mariadb') },
    {
      id: 'redis',
      name: 'Redis / Valkey',
      description: 'An in-memory key-value store for caching, queues, and sessions.',
      logo: logo('redis-valkey'),
      logoDark: logo('redis-valkey-dark'),
    },
    { id: 'keydb', name: 'KeyDB', description: 'A multithreaded, Redis-compatible in-memory store.', logo: logo('keydb'), logoDark: logo('keydb-dark') },
    {
      id: 'dragonfly',
      name: 'Dragonfly',
      description: 'An in-memory datastore compatible with Redis and Memcached.',
      logo: logo('dragonfly'),
      logoDark: logo('dragonfly-dark'),
    },
    { id: 'mongodb', name: 'MongoDB', description: 'A NoSQL database that stores JSON-like documents.', logo: logo('mongodb') },
    { id: 'clickhouse', name: 'ClickHouse', description: 'A column-oriented database for real-time analytics.', logo: logo('clickhouse') },
    { id: 'sqlite', name: 'SQLite', description: 'A lightweight relational database stored in a single file.', logo: logo('sqlite'), experimental: true },
  ];
  const databaseLinks: Record<string, { docs: string; website: string }> = {
    postgresql: { docs: 'https://www.postgresql.org/docs/', website: 'https://www.postgresql.org/?ref=coolify.io' },
    mysql: { docs: 'https://dev.mysql.com/doc/', website: 'https://www.mysql.com/?ref=coolify.io' },
    mariadb: { docs: 'https://mariadb.com/kb/en/documentation/', website: 'https://mariadb.org/?ref=coolify.io' },
    redis: { docs: 'https://redis.io/docs/latest/', website: 'https://redis.io/?ref=coolify.io' },
    keydb: { docs: 'https://docs.keydb.dev/', website: 'https://keydb.dev/?ref=coolify.io' },
    dragonfly: { docs: 'https://www.dragonflydb.io/docs', website: 'https://www.dragonflydb.io/?ref=coolify.io' },
    mongodb: { docs: 'https://www.mongodb.com/docs/', website: 'https://www.mongodb.com/?ref=coolify.io' },
    clickhouse: { docs: 'https://clickhouse.com/docs', website: 'https://clickhouse.com/?ref=coolify.io' },
    sqlite: { docs: 'https://sqlite.org/docs.html', website: 'https://sqlite.org/?ref=coolify.io' },
  };

  type DatabaseImage = { image: string; name: string; badge?: 'default' | 'amd'; description: string; docs: string };
  // The `select-redis-type` step.
  const redisImages: DatabaseImage[] = [
    {
      image: 'valkey/valkey:9-alpine',
      name: 'Valkey 9',
      badge: 'default',
      description: 'An open-source (BSD) fork of Redis from the Linux Foundation. It works with all Redis clients.',
      docs: 'https://hub.docker.com/r/valkey/valkey/',
    },
    { image: 'redis:8', name: 'Redis 8', description: 'The original in-memory key-value store.', docs: 'https://hub.docker.com/_/redis/' },
  ];
  // The `select-postgresql-type` step.
  const postgresImages: DatabaseImage[] = [
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
      image: 'supabase/postgres:17.11.0.018',
      name: 'Supabase PostgreSQL',
      description: 'Supabase is a modern, open-source alternative to PostgreSQL with lots of extensions.',
      docs: 'https://github.com/supabase/postgres',
    },
    {
      image: 'postgis/postgis:18-3.7-alpine',
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

  // `Select::DATABASE_IMAGE_STEPS`: database types that ask for an image before the database is created.
  type ImageStep = 'select-postgresql-type' | 'select-redis-type';
  const databaseImageSteps: Record<string, ImageStep> = { postgresql: 'select-postgresql-type', redis: 'select-redis-type' };
  // `DATABASE_TYPES`.
  const databaseTypes = ['postgresql', 'redis', 'mongodb', 'mysql', 'mariadb', 'keydb', 'dragonfly', 'clickhouse', 'sqlite'];

  let step = $state<'type' | 'servers' | 'destinations' | ImageStep>('type');
  let selectedType = $state('postgresql');
  // Database types coolify-rs can create (SQLite is not ported).
  const portedDatabases = ['postgresql', 'mysql', 'mariadb', 'redis', 'keydb', 'dragonfly', 'mongodb', 'clickhouse'];
  // Application types coolify-rs can create.
  const portedApplications = ['docker-image', 'public', 'private-gh-app', 'private-gitlab-app', 'private-gitea-app', 'private-deploy-key', 'dockerfile', 'docker-compose-empty'];
  // `Create::mount()`: `?type=…&destination=…` renders that type's form.
  const formType = $derived(router.query('type'));
  const formDestination = $derived(router.query('destination') ?? '');
  const formServerId = $derived(router.query('server_id'));
  const formImage = $derived(router.query('database_image') ?? '');
  // `Create::mount()`, `$pendingType`: a database (with its image when it asks for one) or a one-click
  // service is created on this page, which shows what is being created meanwhile.
  const pendingType = $derived.by(() => {
    if (!formType || !formDestination) return null;
    if (databaseTypes.includes(formType)) return databaseImageSteps[formType] && !formImage ? null : formType;
    return formType.startsWith('one-click-service-') ? formType : null;
  });

  // `viewMode`: a list by default; the choice is kept in this browser.
  function storedViewMode(): 'list' | 'grid' {
    try {
      return localStorage.getItem('new-resource-view') === 'grid' ? 'grid' : 'list';
    } catch {
      return 'list';
    }
  }
  let viewMode = $state<'list' | 'grid'>(storedViewMode());
  function setViewMode(mode: 'list' | 'grid') {
    viewMode = mode;
    try {
      localStorage.setItem('new-resource-view', mode);
    } catch {
      // Kept for this page only.
    }
  }
  const viewClasses = $derived(newResourceViewClasses(viewMode));
  const viewModes: ['list' | 'grid', string, string][] = [
    ['list', 'List view', 'unordered-list'],
    ['grid', 'Grid view', 'grid'],
  ];
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
  const servers = $derived((allServers ?? []).filter((s) => s.functional && !s.is_build && s.can_place_resource));
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
  // `filteredServices`: the templates of `Select::loadServices`, by search and category.
  let services = $state<ServiceTemplate[]>([]);
  let categories = $state<string[]>([]);
  let servicesLastUpdated = $state<string | null>(null);
  let category = $state('');
  $effect(() => {
    servicesApi
      .templates()
      .then((r) => {
        services = r.services;
        categories = r.categories;
        servicesLastUpdated = r.last_updated;
      })
      .catch(() => {});
  });
  const shownCategories = $derived(categories.filter((c) => c.toLowerCase().includes(categorySearch.trim().toLowerCase())));
  const filteredServices = $derived(
    services.filter((s) => {
      const q = search.trim().toLowerCase();
      const inSearch =
        q === '' ||
        s.name.toLowerCase().includes(q) ||
        (s.description ?? '').toLowerCase().includes(q) ||
        (s.slogan ?? '').toLowerCase().includes(q) ||
        (s.tags ?? []).some((t) => String(t).toLowerCase().includes(q));
      const inCategory = category === '' || String(s.category ?? '').toLowerCase().includes(category.toLowerCase());
      return inSearch && inCategory;
    }),
  );
  const showServices = $derived((resourceType === 'all' || resourceType === 'services') && filteredServices.length > 0);

  async function setType(type: string) {
    if (selecting) return;
    if (!type.startsWith('one-click-service-') && !portedDatabases.includes(type) && !portedApplications.includes(type)) {
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

  /** `route('project.resource.create', [...])`: this page with the chosen type, destination and server. */
  function createUrl(extra: Record<string, string> = {}): string {
    const query = new URLSearchParams({ type: selectedType, destination: destinationUuid, server_id: String(server?.id ?? ''), ...extra });
    return `/project/${uuid}/environment/${env}/new?${query}`;
  }

  function setDestination(destination: string) {
    destinationUuid = destination;
    // `whatToDoNext()`: PostgreSQL and Redis ask for an image first; everything else goes on.
    const imageStep = databaseImageSteps[selectedType];
    if (imageStep) {
      step = imageStep;
      return;
    }
    router.go(createUrl());
  }

  /** `setDatabaseImage()`. */
  function setDatabaseImage(image: string) {
    if (creating) return;
    creating = true;
    router.go(createUrl({ database_image: image }));
  }

  // `Select::mount()`: with the type, server and destination in the query (the command palette),
  // a database that asks for an image opens on its image step.
  let imageStepFromQuery = false;
  $effect(() => {
    const imageStep = formType ? databaseImageSteps[formType] : undefined;
    if (imageStepFromQuery || !imageStep || !formType || formServerId === null || !formDestination || formImage || allServers === null) return;
    imageStepFromQuery = true;
    // `Gate::allows('placeResource', $server)`: `servers` holds only the servers that pass it.
    const queried = servers.find((s) => String(s.id) === formServerId && s.destinations.some((d) => d.uuid === formDestination));
    if (!queried) return;
    selectedType = formType;
    server = queried;
    destinationUuid = formDestination;
    step = imageStep;
  });

  // `pendingResource()`: the name and logo of what is being created.
  const pendingService = $derived.by(() => {
    if (!pendingType?.startsWith('one-click-service-')) return undefined;
    const key = pendingType.slice('one-click-service-'.length).toLowerCase();
    return services.find((s) => s.id.toLowerCase() === key);
  });
  /** `Str::headline()` of a type or a template key. */
  const headline = (value: string) =>
    value
      .split(/[-_\s]+/)
      .filter(Boolean)
      .map((word) => word.charAt(0).toUpperCase() + word.slice(1))
      .join(' ');
  const pendingName = $derived(
    !pendingType ? '' : pendingType.startsWith('one-click-service-') ? (pendingService?.name ?? headline(pendingType.slice('one-click-service-'.length))) : headline(pendingType),
  );
  const pendingLogo = $derived(pendingService?.logo ?? null);

  // `wire:init="createResource"`: creates the pending resource once and opens it.
  let createdFor = '';
  $effect(() => {
    if (!project || !pendingType) return;
    const request = `${pendingType}|${formDestination}|${formImage}`;
    if (createdFor === request) return;
    createdFor = request;
    createResource(pendingType, formDestination, formImage);
  });

  /** `Create::createResource()`. The new page replaces this one, so Back does not create a second resource. */
  async function createResource(type: string, destination: string, image: string) {
    try {
      if (type.startsWith('one-click-service-')) {
        const r = await servicesApi.create({ project_uuid: uuid, environment_uuid: env, destination_uuid: destination, type });
        router.go(`/project/${uuid}/environment/${env}/service/${r.uuid}`, true);
        return;
      }
      const { uuid: dbUuid } = await dbUi.create({
        project_uuid: uuid,
        environment_uuid: env,
        destination_uuid: destination,
        type,
        ...(image ? { database_image: image } : {}),
      });
      router.go(`/project/${uuid}/environment/${env}/database/${dbUuid}`, true);
    } catch (e) {
      notify.error(e instanceof ApiError ? e.message : String(e));
      creating = false;
      step = 'type';
      router.go(`/project/${uuid}/environment/${env}/new`, true);
    }
  }

  function reload() {
    loadServers();
  }

  const row =
    'group flex min-h-14 w-full items-center gap-3 px-4 py-3 text-left transition-colors hover:bg-neutral-50 dark:hover:bg-white/[0.025]';
  const rowIcon =
    'flex size-8 shrink-0 items-center justify-center rounded-lg border border-neutral-200 bg-neutral-50 text-neutral-500 dark:border-white/[0.08] dark:bg-white/[0.035] dark:text-fg-dim';
  const imageCard =
    'group relative flex min-h-24 items-center gap-3 rounded-xl border border-neutral-200 bg-white p-4 transition-colors hover:border-neutral-300 dark:border-white/[0.08] dark:bg-white/[0.05] dark:hover:border-white/[0.14]';
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

<!-- One card of an image step (`select-postgresql-type`, `select-redis-type`). -->
{#snippet imageOption(option: DatabaseImage)}
  <div
    class="{imageCard} {creating ? 'cursor-not-allowed opacity-50' : 'cursor-pointer'}"
    role="button"
    tabindex="-1"
    aria-disabled={creating}
    onclick={() => setDatabaseImage(option.image)}
    onkeydown={(e) => e.key === 'Enter' && setDatabaseImage(option.image)}
  >
    <div class="flex flex-col">
      <div class="text-[13px] font-semibold text-black dark:text-fg">
        {option.name}
        {#if option.badge === 'default'}
          <span class="ml-1 rounded-full bg-coollabs/10 px-2 py-0.5 text-[10px] font-medium text-coollabs dark:bg-brand/15 dark:text-brand-light">Default</span>
        {:else if option.badge === 'amd'}
          <span class="ml-1 text-[10px] font-medium text-amber-600 dark:text-amber-300">AMD only</span>
        {/if}
      </div>
      <div class="mt-1 pr-8 text-[11px] leading-4 text-neutral-500 dark:text-fg-faint">{option.description}</div>
    </div>
    <a
      href={option.docs}
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
{/snippet}

{#if missing}
  <div class="application-settings-form w-full">
    <Empty title="Environment not found" description="It may have been deleted, or it belongs to another team." iconName="layers" size="lg" />
  </div>
{:else if project && pendingType}
  <div>
    <div class="mx-auto flex max-w-md flex-col items-center pt-24 text-center">
      {#if pendingLogo}
        <img src={pendingLogo} alt="" class="mb-4 size-10 object-contain" />
      {:else}
        <Reicon name="database" class="mb-4 size-8 text-neutral-400 dark:text-fg-faint" />
      {/if}
      <h2 class="empty-state-title">Creating {pendingName}</h2>
      <p class="empty-state-description text-[13px]">Setting up the resource. You will be redirected when it is ready.</p>
      <Loading text="Creating..." class="mt-4" />
    </div>
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
                <!-- Only the view that is not current shows, as an icon: the button switches to that view. -->
                <div class="view-toggle w-fit">
                  {#each viewModes as [mode, viewLabel, icon] (mode)}
                    {#if viewMode !== mode}
                      <button
                        type="button"
                        onclick={() => setViewMode(mode)}
                        title={viewLabel}
                        aria-label={viewLabel}
                        class="relative flex h-7.5 w-7.5 items-center justify-center rounded-md text-neutral-400 transition-colors hover:bg-neutral-100 hover:text-black dark:hover:text-white"
                      >
                        <Reicon name={icon} class="size-3.5" />
                      </button>
                    {/if}
                  {/each}
                </div>

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

                <!-- Service categories come from the templates (`Select::loadServices`). -->
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
                    title={category || 'All categories'}
                  >
                    <span class="listbox-trigger-label capitalize">{category || 'All categories'}</span>
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
                          aria-selected={category === ''}
                          onclick={() => {
                            category = '';
                            categorySearch = '';
                            categoryOpen = false;
                          }}
                        >
                          <span>All categories</span>
                          {#if category === ''}<Reicon name="check" class="size-3.5 text-black dark:text-fg" />{/if}
                        </button>
                        {#each shownCategories as c (c)}
                          <button
                            type="button"
                            class="listbox-option"
                            role="option"
                            aria-selected={category === c}
                            onclick={() => {
                              category = c;
                              categorySearch = '';
                              categoryOpen = false;
                            }}
                          >
                            <span class="capitalize">{c}</span>
                            {#if category === c}<Reicon name="check" class="size-3.5 text-black dark:text-fg" />{/if}
                          </button>
                        {/each}
                      </div>
                    </div>
                  {/if}
                </div>
              </div>
            </div>
          </SettingsSection>
          <!-- A full page load: the canvas uses the minimal layout. -->
          {#if search === ''}
            <a
              href="/canvas?project={uuid}&environment={env}"
              class="mt-6 flex items-center gap-4 rounded-xl border border-neutral-200 bg-white p-4 shadow-[0_1px_2px_rgb(0_0_0/0.05)] transition-colors hover:border-neutral-300 focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-accent dark:border-white/[0.08] dark:bg-white/[0.05] dark:shadow-none dark:hover:border-white/[0.14]"
            >
              <div class="flex size-11 shrink-0 items-center justify-center rounded-lg border border-neutral-200 bg-neutral-50 text-neutral-600 dark:border-white/[0.08] dark:bg-white/[0.04] dark:text-fg-dim">
                <Reicon name="deploy-canvas" class="size-5" />
              </div>
              <div class="min-w-0 flex-1">
                <div class="flex items-center gap-2">
                  <h2 class="text-[14px]! leading-5! font-semibold! text-black! dark:text-fg!">Deploy canvas</h2>
                  <BetaBadge />
                </div>
                <p class="mt-0.5 text-[12px] leading-5 text-neutral-600 dark:text-fg-dim">
                  Connect a server, a source, and its settings step by step, then deploy every resource at once.
                </p>
              </div>
              <span class="button button-highlighted shrink-0 max-sm:hidden">
                Try it
                <Reicon name="arrow-right" class="size-3.5" />
              </span>
            </a>
          {/if}

          <div class="mt-6 flex flex-col gap-6">
            {#if showApplications}
              <section class="application-settings-section">
                <div class="application-settings-section-header">
                  <div class="flex items-center gap-2">
                    <Reicon name="globe" class="size-4 text-neutral-400 dark:text-fg-faint" />
                    <h2>Applications</h2>
                  </div>
                </div>
                <div class="application-settings-section-body {viewMode === 'list' ? 'is-flush' : ''}">
                  <div class="text-left {viewClasses.container}">
                    {#each filteredGit as app (app.name)}
                      <NewResourceCard
                        name={app.name}
                        description={app.description}
                        logo={app.logo}
                        logoDark={app.logoDark}
                        source="Git source"
                        docs={app.documentation}
                        view={viewClasses}
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
                        view={viewClasses}
                        onselect={() => setType(app.id)}
                      />
                    {/each}
                  </div>
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
                <div class="application-settings-section-body {viewMode === 'list' ? 'is-flush' : ''}">
                  <div class="text-left {viewClasses.container}">
                    {#each filteredDatabases as database (database.id)}
                      <NewResourceCard
                        name={database.name}
                        description={database.description}
                        logo={database.logo}
                        logoDark={database.logoDark}
                        experimental={database.experimental}
                        docs={databaseLinks[database.id]?.docs}
                        website={databaseLinks[database.id]?.website}
                        view={viewClasses}
                        onselect={() => setType(database.id)}
                      />
                    {/each}
                  </div>
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
                  <div class="flex flex-wrap items-center gap-2">
                    {#if servicesLastUpdated}
                      <div class="text-[11px] text-neutral-500 dark:text-fg-faint">
                        Updated <span>{servicesLastUpdated}</span>
                      </div>
                    {/if}
                  </div>
                </div>
                <div class="application-settings-section-body {viewMode === 'list' ? 'is-flush' : ''}">
                  <div class={viewMode === 'list' ? 'px-4 pt-4' : ''}>
                    <Callout type="info" title="Trademarks policy" class="mb-4">
                      The respective trademarks mentioned here are owned by the respective companies, and use of them does not imply any affiliation or
                      endorsement.
                    </Callout>
                  </div>
                  <div class="text-left {viewClasses.container}">
                    {#each filteredServices as service (service.id)}
                      <article
                        role="button"
                        tabindex="0"
                        aria-label="Configure {service.name}"
                        onclick={() => setType('one-click-service-' + service.id)}
                        onkeydown={(e) => {
                          if (e.target === e.currentTarget && (e.key === 'Enter' || e.key === ' ')) {
                            e.preventDefault();
                            setType('one-click-service-' + service.id);
                          }
                        }}
                        class="group flex cursor-pointer transition-colors focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-accent {viewClasses.item}"
                      >
                        <div class="flex min-w-0 items-start gap-3 {viewClasses.header}">
                          <div class="flex shrink-0 items-center justify-center overflow-hidden border border-neutral-200 bg-neutral-50 dark:border-white/[0.08] dark:bg-white/[0.04] {viewClasses.logo}">
                            <img
                              class="h-full w-full object-contain p-2"
                              src={service.logo}
                              alt=""
                              onerror={(e) => {
                                const img = e.currentTarget as HTMLImageElement;
                                if (!img.dataset.cdnTried) {
                                  img.dataset.cdnTried = 'true';
                                  img.src = service.logo_cdn_url;
                                } else if (!img.dataset.defaultTried) {
                                  img.dataset.defaultTried = 'true';
                                  img.src = service.logo_default_url;
                                }
                              }}
                            />
                          </div>
                          <div class="min-w-0 flex-1">
                            <h3 class="truncate text-[13px] font-semibold text-black dark:text-fg">{service.name}</h3>
                            <p class="mt-0.5 truncate text-[11px] text-neutral-500 dark:text-fg-faint">
                              {#if service.templateLastUpdated}<span>Updated </span>{/if}
                              <span>{service.templateLastUpdated || 'Template ready'}</span>
                            </p>
                          </div>
                          {#if service.amd_only || service.arm_only}
                            <span class="shrink-0 rounded-md border border-amber-300/50 bg-amber-50 px-1.5 py-0.5 text-[10px] font-medium text-amber-700 dark:border-warning/20 dark:bg-warning/10 dark:text-warning"
                              >{service.arm_only ? 'ARM only' : 'AMD only'}</span
                            >
                          {/if}
                        </div>
                        <p class="text-[12px] leading-5 text-neutral-600 dark:text-fg-dim {viewClasses.description}">
                          {service.slogan || service.description || 'Deploy this service with a ready-to-use coolify-rs template.'}
                        </p>
                        <div class="flex items-center gap-1.5 {viewClasses.footer}">
                          <a
                            href={'https://coolify.io/docs/services/' + service.docsSlug}
                            target="_blank"
                            rel="noopener noreferrer"
                            onclick={(e) => e.stopPropagation()}
                            class="icon-button"
                            title="Documentation"
                            aria-label="Documentation"
                          >
                            <Reicon name="documentation" class="size-4" />
                          </a>
                          {#if service.website}
                            <a
                              href={service.website}
                              target="_blank"
                              rel="noopener noreferrer"
                              class="icon-button"
                              title="Website"
                              aria-label="Website"
                              onclick={(e) => e.stopPropagation()}
                            >
                              <Reicon name="globe" class="size-4" />
                            </a>
                          {/if}
                          <span class="button {viewClasses.action}">
                            Configure
                            <Reicon name="arrow-right" class="size-3.5" />
                          </span>
                        </div>
                      </article>
                    {/each}
                  </div>
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
                <StatusBadge type="success" label="Running" />
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
                <StatusBadge label="Build only" />
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
                <StatusBadge type="success" label="Standalone Docker" />
              </button>
            {/each}
          </div>
        </SettingsSection>
      {/if}

      {#if step === 'select-redis-type'}
        <div>
          <div class="mb-4">
            <h2 class="text-[15px]! font-semibold!">Select a Redis or Valkey image</h2>
            <p class="mt-1 text-[12px] text-neutral-500 dark:text-fg-dim">Both work with the same clients, commands, and configuration.</p>
          </div>
          <div class="grid grid-cols-1 gap-3 lg:grid-cols-2">
            {#each redisImages as option (option.image)}{@render imageOption(option)}{/each}
          </div>
        </div>
      {/if}

      {#if step === 'select-postgresql-type'}
        <div>
          <div class="mb-4">
            <h2 class="text-[15px]! font-semibold!">Select a PostgreSQL image</h2>
            <p class="mt-1 text-[12px] text-neutral-500 dark:text-fg-dim">Use PostgreSQL 18 unless the workload needs bundled extensions.</p>
          </div>
          <div class="grid grid-cols-1 gap-3 lg:grid-cols-2">
            {#each postgresImages as option (option.image)}{@render imageOption(option)}{/each}
          </div>
        </div>
      {/if}
    </div>
  </div>
{/if}
