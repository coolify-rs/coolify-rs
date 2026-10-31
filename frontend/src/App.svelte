<script lang="ts">
  import Shell from './components/Shell.svelte';
  import Toasts from './components/Toasts.svelte';
  import Login from './pages/Login.svelte';
  import Dashboard from './pages/Dashboard.svelte';
  import Projects from './pages/Projects.svelte';
  import Project from './pages/Project.svelte';
  import ProjectEdit from './pages/ProjectEdit.svelte';
  import Environment from './pages/Environment.svelte';
  import EnvironmentEdit from './pages/EnvironmentEdit.svelte';
  import Keys from './pages/Keys.svelte';
  import Key from './pages/Key.svelte';
  import Team from './pages/Team.svelte';
  import Members from './pages/Members.svelte';
  import Appearance from './pages/Appearance.svelte';
  import DockerRegistries from './pages/DockerRegistries.svelte';
  import Destinations from './pages/Destinations.svelte';
  import Destination from './pages/Destination.svelte';
  import Servers from './pages/Servers.svelte';
  import ServerNew from './pages/ServerNew.svelte';
  import Server from './pages/Server.svelte';
  import ServerDanger from './pages/ServerDanger.svelte';
  import ServerSection from './pages/ServerSection.svelte';
  import ResourceNew from './pages/ResourceNew.svelte';
  import Database from './pages/Database.svelte';
  import Application from './pages/Application.svelte';
  import Unported from './pages/Unported.svelte';
  import { router } from './lib/router.svelte';
  import { session } from './lib/session.svelte';
  import { page } from './lib/page.svelte';
  import { setUnauthenticatedHandler } from './lib/api';

  session.refresh();
  setUnauthenticatedHandler(() => {
    session.set(null);
    router.go('/login', true);
  });

  // Route guard: signed-out users see the login page; signed-in users skip it.
  $effect(() => {
    if (!session.loaded) return;
    const name = router.current.name;
    if (!session.info && name !== 'login') router.go('/login', true);
    else if (session.info && name === 'login') router.go('/', true);
  });

  // Breadcrumb switchers belong to the page that set them.
  $effect.pre(() => {
    void router.path;
    page.switchers = [];
    page.navContext = null;
    page.server = null;
  });

  // Pages that show a named record set their own title once it loads.
  const titles: Record<string, string> = {
    login: 'Login',
    dashboard: 'Dashboard',
    projects: 'Projects',
    keys: 'Keys & Tokens',
    team: 'Teams',
    members: 'Team Members',
    appearance: 'Appearance',
    destinations: 'Destinations',
    registries: 'Docker Registries',
    servers: 'Servers',
    'server-new': 'New Server',
    'server-new-type': 'New Server',
  };
  $effect(() => {
    const name = router.current.name;
    if (titles[name]) document.title = `${titles[name]} | coolify-rs`;
    else if (name === 'unported') document.title = 'coolify-rs';
  });

  const params = $derived(router.current.params);
</script>

{#if !session.loaded}
  <main class="h-full bg-neutral-50 dark:bg-base"></main>
{:else if !session.info || router.current.name === 'login'}
  <Login />
{:else}
  <Shell>
    {#if session.info.current_team?.two_factor_blocked}
      <Unported
        title="Two-factor authentication required"
        description="This team requires two-factor authentication. Enable it for your account in Coolify, then sign in again."
        iconName="keys"
      />
    {:else if router.current.name === 'dashboard'}
      <Dashboard />
    {:else if router.current.name === 'projects'}
      <Projects />
    {:else if router.current.name === 'project'}
      {#key params.uuid}<Project uuid={params.uuid} />{/key}
    {:else if router.current.name === 'project-edit'}
      {#key params.uuid}<ProjectEdit uuid={params.uuid} />{/key}
    {:else if router.current.name === 'environment'}
      {#key router.path}<Environment uuid={params.uuid} env={params['env']} />{/key}
    {:else if router.current.name === 'environment-edit'}
      {#key router.path}<EnvironmentEdit uuid={params.uuid} env={params['env']} />{/key}
    {:else if router.current.name === 'resource-new'}
      {#key router.path + router.search}<ResourceNew uuid={params.uuid} env={params['env']} />{/key}
    {:else if router.current.name === 'database' || router.current.name === 'database-section'}
      {#key router.path}<Database uuid={params.uuid} env={params['env']} db={params['db']} section={params['section'] ?? 'general'} />{/key}
    {:else if router.current.name === 'application' || router.current.name === 'application-section'}
      {#key router.path}<Application uuid={params.uuid} env={params['env']} app={params['app']} section={params['section'] ?? 'general'} />{/key}
    {:else if router.current.name === 'application-deployment'}
      {#key params['app']}<Application uuid={params.uuid} env={params['env']} app={params['app']} section="deployment" dep={params['dep']} />{/key}
    {:else if router.current.name === 'keys'}
      <Keys />
    {:else if router.current.name === 'key'}
      {#key params.uuid}<Key uuid={params.uuid} />{/key}
    {:else if router.current.name === 'team'}
      <Team />
    {:else if router.current.name === 'members'}
      <Members />
    {:else if router.current.name === 'appearance'}
      <Appearance />
    {:else if router.current.name === 'registries'}
      <DockerRegistries />
    {:else if router.current.name === 'destinations'}
      <Destinations />
    {:else if router.current.name === 'destination'}
      {#key router.path}<Destination uuid={params.uuid} section={params.section ?? 'general'} />{/key}
    {:else if router.current.name === 'servers'}
      <Servers />
    {:else if router.current.name === 'server-new' || router.current.name === 'server-new-type'}
      {#key router.path}<ServerNew type={params.type} />{/key}
    {:else if router.current.name === 'server'}
      {#key params.uuid}<Server uuid={params.uuid} />{/key}
    {:else if router.current.name === 'server-section' && params.section === 'danger'}
      {#key params.uuid}<ServerDanger uuid={params.uuid} />{/key}
    {:else if router.current.name === 'server-section'}
      {#key router.path}<ServerSection uuid={params.uuid} section={params.section} sub={params.sub} />{/key}
    {:else}
      <Unported />
    {/if}
  </Shell>
{/if}

<Toasts />
