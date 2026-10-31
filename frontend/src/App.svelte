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
  import TeamDanger from './pages/TeamDanger.svelte';
  import TeamAudit from './pages/TeamAudit.svelte';
  import TeamAdmin from './pages/TeamAdmin.svelte';
  import SelectTeam from './pages/SelectTeam.svelte';
  import Invitation from './pages/Invitation.svelte';
  import ConfirmPassword from './pages/ConfirmPassword.svelte';
  import Profile from './pages/Profile.svelte';
  import PasskeyConfirm from './pages/PasskeyConfirm.svelte';
  import ErrorPage from './components/ErrorPage.svelte';
  import Servers from './pages/Servers.svelte';
  import ServerNew from './pages/ServerNew.svelte';
  import Server from './pages/Server.svelte';
  import ServerDanger from './pages/ServerDanger.svelte';
  import ServerSection from './pages/ServerSection.svelte';
  import ResourceNew from './pages/ResourceNew.svelte';
  import Database from './pages/Database.svelte';
  import Application from './pages/Application.svelte';
  import NotificationEmail from './pages/NotificationEmail.svelte';
  import NotificationChannel from './pages/NotificationChannel.svelte';
  import Tags from './pages/Tags.svelte';
  import SharedVariables from './pages/SharedVariables.svelte';
  import SharedVariableOwners from './pages/SharedVariableOwners.svelte';
  import SharedVariableScope from './pages/SharedVariableScope.svelte';
  import Storages from './pages/Storages.svelte';
  import Storage from './pages/Storage.svelte';
  import ApiTokens from './pages/ApiTokens.svelte';
  import CloudTokens from './pages/CloudTokens.svelte';
  import CloudToken from './pages/CloudToken.svelte';
  import CloudInitScripts from './pages/CloudInitScripts.svelte';
  import CloudInitScript from './pages/CloudInitScript.svelte';
  import IntegrationTokens from './pages/IntegrationTokens.svelte';
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

  // `redirect()->guest()`: an invitation comes back after signing in.
  let intended: string | null = null;

  // Route guard: signed-out users see the login page; signed-in users skip it.
  $effect(() => {
    if (!session.loaded) return;
    const name = router.current.name;
    if (name === 'auth-link') return;
    if (!session.info && name !== 'login') {
      if (name === 'invitation') intended = router.path;
      router.go('/login', true);
    } else if (session.info && intended) {
      const to = intended;
      intended = null;
      router.go(to, true);
    } else if (session.info && name === 'login') router.go('/', true);
    // `DecideWhatToDoWithUser`: no active team and several to choose from.
    else if (session.info && !session.info.current_team && !['select-team', 'invitation', 'confirm-password', 'profile', 'profile-passkey-confirm'].includes(name)) router.go('/select-team', true);
  });

  // Breadcrumb switchers belong to the page that set them.
  $effect.pre(() => {
    void router.path;
    page.switchers = [];
    page.resourceSwitcher = null;
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
    'team-danger': 'Team Danger Zone',
    'team-audit': 'Team Audit Log',
    'team-admin': 'Team Admin',
    'select-team': 'Select a team',
    'confirm-password': 'Confirm Password',
    profile: 'Profile',
    appearance: 'Appearance',
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
{:else if router.current.name === 'auth-link'}
  <Invitation token={router.query('token') ?? ''} />
{:else if !session.info || router.current.name === 'login'}
  <Login />
{:else if router.current.name === 'select-team'}
  <SelectTeam />
{:else if router.current.name === 'invitation'}
  {#key params.uuid}<Invitation uuid={params.uuid} />{/key}
{:else if router.current.name === 'confirm-password'}
  <ConfirmPassword />
{:else if router.current.name === 'profile-passkey-confirm'}
  <PasskeyConfirm />
{:else if (router.current.name === 'team-danger' && !session.isOwner) || (router.current.name === 'team-audit' && !session.isAdmin)}
  <ErrorPage code={403} />
{:else}
  <Shell>
    {#if router.current.name === 'profile'}
      <Profile />
    {:else if session.info.current_team?.two_factor_blocked}
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
    {:else if router.current.name === 'team-danger'}
      <TeamDanger />
    {:else if router.current.name === 'team-audit'}
      <TeamAudit />
    {:else if router.current.name === 'team-admin'}
      <TeamAdmin />
    {:else if router.current.name === 'appearance'}
      <Appearance />
    {:else if router.current.name === 'notification-email'}
      <NotificationEmail />
    {:else if router.current.name === 'notification-channel'}
      {#key params.channel}<NotificationChannel channel={params.channel} />{/key}
    {:else if router.current.name === 'tags'}
      {#key router.path}<Tags tagName={params.tagName} />{/key}
    {:else if router.current.name === 'shared-overview'}
      <SharedVariables />
    {:else if ['shared-projects', 'shared-environments', 'shared-servers'].includes(router.current.name)}
      {#key router.path}<SharedVariableOwners kind={router.current.name} />{/key}
    {:else if router.current.name === 'shared-team'}
      <SharedVariableScope kind="team" />
    {:else if router.current.name === 'project-shared-variables'}
      {#key router.path}<SharedVariableScope kind="project" uuid={params.uuid} />{/key}
    {:else if router.current.name === 'environment-shared-variables'}
      {#key router.path}<SharedVariableScope kind="environment" uuid={params.uuid} env={params['env']} />{/key}
    {:else if router.current.name === 'shared-server'}
      {#key router.path}<SharedVariableScope kind="server" uuid={params.uuid} />{/key}
    {:else if router.current.name === 'storages'}
      <Storages />
    {:else if router.current.name === 'storage'}
      {#key router.path}<Storage uuid={params.uuid} section={params.section ?? 'general'} />{/key}
    {:else if router.current.name === 'api-tokens'}
      <ApiTokens />
    {:else if router.current.name === 'cloud-tokens'}
      <CloudTokens />
    {:else if router.current.name === 'cloud-token'}
      {#key params.uuid}<CloudToken uuid={params.uuid} />{/key}
    {:else if router.current.name === 'cloud-init-scripts'}
      <CloudInitScripts />
    {:else if router.current.name === 'cloud-init-script'}
      {#key params.uuid}<CloudInitScript uuid={params.uuid} />{/key}
    {:else if router.current.name === 'integration-tokens'}
      <IntegrationTokens />
    {:else if router.current.name === 'servers'}
      <Servers />
    {:else if router.current.name === 'server-new' || router.current.name === 'server-new-type'}
      {#key router.path}<ServerNew type={params.type} />{/key}
    {:else if router.current.name === 'server'}
      {#key params.uuid}<Server uuid={params.uuid} />{/key}
    {:else if router.current.name === 'server-section' && params.section === 'danger'}
      {#key params.uuid}<ServerDanger uuid={params.uuid} />{/key}
    {:else if router.current.name === 'server-section'}
      {#key router.path}<ServerSection uuid={params.uuid} section={params.section} />{/key}
    {:else}
      <Unported />
    {/if}
  </Shell>
{/if}

<Toasts />
