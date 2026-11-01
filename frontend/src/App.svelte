<script lang="ts">
  import Shell from './components/Shell.svelte';
  import Toasts from './components/Toasts.svelte';
  import Login from './pages/Login.svelte';
  import Analytics from './pages/Analytics.svelte';
  import Register from './pages/Register.svelte';
  import ForgotPassword from './pages/ForgotPassword.svelte';
  import ResetPassword from './pages/ResetPassword.svelte';
  import TwoFactorChallenge from './pages/TwoFactorChallenge.svelte';
  import ForcePasswordReset from './pages/ForcePasswordReset.svelte';
  import TwoFactorRequired from './pages/TwoFactorRequired.svelte';
  import VerifyEmail from './pages/VerifyEmail.svelte';
  import EmailVerifyLink from './pages/EmailVerifyLink.svelte';
  import SettingsOauth from './pages/SettingsOauth.svelte';
  import Dashboard from './pages/Dashboard.svelte';
  import Projects from './pages/Projects.svelte';
  import Project from './pages/Project.svelte';
  import ProjectEdit from './pages/ProjectEdit.svelte';
  import Environment from './pages/Environment.svelte';
  import EnvironmentEdit from './pages/EnvironmentEdit.svelte';
  import CloneMe from './pages/CloneMe.svelte';
  import Keys from './pages/Keys.svelte';
  import Key from './pages/Key.svelte';
  import Team from './pages/Team.svelte';
  import Members from './pages/Members.svelte';
  import Appearance from './pages/Appearance.svelte';
  import DockerRegistries from './pages/DockerRegistries.svelte';
  import Destinations from './pages/Destinations.svelte';
  import Destination from './pages/Destination.svelte';
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
  import Sources from './pages/Sources.svelte';
  import SourceGithub from './pages/SourceGithub.svelte';
  import SourceOauth from './pages/SourceOauth.svelte';
  import Database from './pages/Database.svelte';
  import { canvasRedirect } from './lib/canvasView';
  import Admin from './pages/Admin.svelte';
  import SubscriptionPage from './pages/SubscriptionPage.svelte';
  import CanvasPage from './pages/CanvasPage.svelte';
  import DeployCanvasPage from './pages/DeployCanvasPage.svelte';
  import Service from './pages/Service.svelte';
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
  import SettingsGeneral from './pages/SettingsGeneral.svelte';
  import SettingsAdvanced from './pages/SettingsAdvanced.svelte';
  import SettingsUpdates from './pages/SettingsUpdates.svelte';
  import SettingsEmail from './pages/SettingsEmail.svelte';
  import SettingsBackup from './pages/SettingsBackup.svelte';
  import Onboarding from './pages/Onboarding.svelte';
  import Terminal from './pages/Terminal.svelte';
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

  // Pages behind `guest`: a signed-in user goes to the dashboard.
  const guestPages = ['login', 'register', 'forgot-password', 'reset-password', 'two-factor-challenge'];
  // `allowedPathsForTwoFactorSetup()`.
  const twoFactorSetup = ['two-factor-required', 'confirm-password'];
  // `allowedPathsForBoardingAccounts()` with the pages behind them.
  const boardingAllowed = ['onboarding', 'force-password-reset', 'profile', 'appearance', 'profile-passkey-confirm', ...twoFactorSetup];

  // The web middleware group, in order: `Authenticate`, `CheckForcePasswordReset`,
  // `DecideWhatToDoWithUser` (team selection, then boarding; the email and subscription checks
  // only apply to Coolify Cloud), `RequireTeamTwoFactor`.
  $effect(() => {
    if (!session.loaded) return;
    const name = router.current.name;
    if (name === 'auth-link' || name === 'oauth-denied') return;
    const info = session.info;
    if (!info) {
      if (!guestPages.includes(name)) {
        // `redirect()->guest()`: the page comes back after signing in.
        if (router.path !== '/') intended = router.path + router.search;
        router.go('/login', true);
      }
      return;
    }
    if (intended) {
      const to = intended;
      intended = null;
      router.go(to, true);
    } else if (guestPages.includes(name)) router.go('/', true);
    else if (info.force_password_reset) {
      if (name !== 'force-password-reset') router.go('/force-password-reset', true);
    } else if (name === 'force-password-reset') router.go('/', true);
    else if (!info.current_team && !['select-team', 'invitation', 'confirm-password', 'profile', 'profile-passkey-confirm'].includes(name)) router.go('/select-team', true);
    else if (info.show_boarding && !boardingAllowed.includes(name)) router.go('/onboarding', true);
    else if (info.current_team?.two_factor_blocked && !twoFactorSetup.includes(name)) router.go('/two-factor-required', true);
  });

  // Breadcrumb switchers belong to the page that set them.
  $effect.pre(() => {
    void router.path;
    page.switchers = [];
    page.resourceSwitcher = null;
    page.navContext = null;
    page.server = null;
    page.sourceConnected = null;
    page.navChildren = null;
  });

  // Pages that show a named record set their own title once it loads.
  const titles: Record<string, string> = {
    login: 'Login',
    register: 'Register',
    'forgot-password': 'Forgot Password',
    'reset-password': 'Reset Password',
    'two-factor-challenge': 'Two Factor Challenge',
    'force-password-reset': 'Set a new password',
    'two-factor-required': 'Two-factor authentication required',
    verify: 'Verify Email',
    'settings-oauth': 'Authentication',
    dashboard: 'Dashboard',
    analytics: 'Analytics',
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
    destinations: 'Destinations',
    registries: 'Docker Registries',
    sources: 'Sources',
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
  const canvasKinds: Record<string, string> = {
    'canvas-tags': 'tags',
    'canvas-dashboard': 'dashboard',
    'canvas-projects': 'projects',
    'canvas-project': 'project',
    'canvas-environment': 'environment',
    'canvas-servers': 'servers',
    'canvas-server': 'server',
    'canvas-team-variables': 'team_variables',
    'canvas-project-variables': 'project_variables',
    'canvas-environment-variables': 'environment_variables',
    'canvas-server-variables': 'server_variables',
  };
  const fullBleed = $derived(router.current.name.startsWith('canvas-'));
  // `RemembersCanvasView`: a list page opens the canvas when the user left it there.
  $effect.pre(() => {
    if (!session.info) return;
    // `Route::any('/{any}')`: every other path goes home.
    const devOnly = (router.current.name === 'dev-error' || router.current.name === 'dev-exception') && !session.info.features.dev;
    if (router.current.name === 'unported' || devOnly || (router.current.name === 'realtime' && session.info.current_team?.id !== 0)) {
      router.go('/', true);
      return;
    }
    const to = canvasRedirect(router.current.name, router.current.params);
    if (to) router.go(to, true);
  });
  // `can.access.terminal` on the terminal page of a resource or server.
  const isTerminalSection = $derived(params.section === 'terminal' && /-section$|^server/.test(router.current.name));
</script>

{#if !session.loaded}
  <main class="h-full bg-neutral-50 dark:bg-base"></main>
{:else if router.current.name === 'auth-link'}
  <Invitation token={router.query('token') ?? ''} />
{:else if router.current.name === 'oauth-denied'}
  <ErrorPage code={403} />
{:else if !session.info && router.current.name === 'register'}
  <Register />
{:else if !session.info && router.current.name === 'forgot-password'}
  <ForgotPassword />
{:else if !session.info && router.current.name === 'reset-password'}
  <ResetPassword token={params.token} email={router.query('email') ?? ''} />
{:else if !session.info && router.current.name === 'two-factor-challenge'}
  <TwoFactorChallenge />
{:else if !session.info || guestPages.includes(router.current.name)}
  <Login />
{:else if router.current.name === 'force-password-reset'}
  <ForcePasswordReset />
{:else if router.current.name === 'two-factor-required'}
  <TwoFactorRequired />
{:else if router.current.name === 'verify'}
  <VerifyEmail />
{:else if router.current.name === 'email-verify'}
  <EmailVerifyLink />
{:else if router.current.name === 'realtime' && session.info.current_team?.id === 0}
  <!-- `Controller::realtime_test`: `TestEvent` has no listener here, since the UI has no websocket. -->
  Look at your other tab.
{:else if router.current.name === 'dev-error' && session.info.features.dev}
  <ErrorPage code={Number(params.code) as never} />
{:else if router.current.name === 'dev-exception' && session.info.features.dev}
  <ErrorPage code={500} description="Testing Laravel exception page" />
{:else if router.current.name === 'select-team'}
  <SelectTeam />
{:else if router.current.name === 'onboarding'}
  <Onboarding />
{:else if router.current.name === 'invitation'}
  {#key params.uuid}<Invitation uuid={params.uuid} />{/key}
{:else if router.current.name === 'confirm-password'}
  <ConfirmPassword />
{:else if router.current.name === 'profile-passkey-confirm'}
  <PasskeyConfirm />
{:else if router.current.name === 'admin' && !((session.info.features.cloud || session.info.features.dev) && (session.info.user.id === 0 || session.info.features.impersonating))}
  <!-- `Admin\\Index::mount`: abort(403). -->
  <ErrorPage code={403} />
{:else if (router.current.name === 'team-danger' && !session.isOwner) || ((router.current.name === 'team-audit' || router.current.name === 'terminal' || isTerminalSection) && !session.isAdmin)}
  <ErrorPage code={403} />
{:else}
  <Shell {fullBleed}>
    {#if router.current.name === 'admin'}
      <Admin />
    {:else if router.current.name === 'subscription' || router.current.name === 'subscription-new'}
      {#key router.path}<SubscriptionPage page={router.current.name === 'subscription' ? 'show' : 'index'} />{/key}
    {:else if router.current.name === 'canvas-deploy'}
      {#key router.path}<DeployCanvasPage />{/key}
    {:else if canvasKinds[router.current.name]}
      {#key router.path}<CanvasPage kind={canvasKinds[router.current.name] as never} uuid={params.uuid} environment={params['env']} />{/key}
    {:else if router.current.name === 'profile'}
      <Profile />
    {:else if router.current.name === 'dashboard'}
      <Dashboard />
    {:else if router.current.name === 'analytics'}
      <Analytics />
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
    {:else if router.current.name === 'environment-clone'}
      {#key router.path}<CloneMe uuid={params.uuid} env={params['env']} />{/key}
    {:else if router.current.name === 'resource-new'}
      {#key router.path + router.search}<ResourceNew uuid={params.uuid} env={params['env']} />{/key}
    {:else if router.current.name === 'database' || router.current.name === 'database-section' || router.current.name === 'database-backup'}
      {#key router.path}<Database uuid={params.uuid} env={params['env']} db={params['db']} section={params['section'] ?? (params['backup'] ? 'backups' : 'general')} backup={params['backup']} backupSection={params['backupSection']} />{/key}
    {:else if router.current.name === 'service' || router.current.name === 'service-section' || router.current.name === 'service-import-backup' || router.current.name === 'service-volume-backup' || router.current.name === 'service-database-backups' || router.current.name === 'service-database-import'}
      {#key router.path}<Service uuid={params.uuid} env={params['env']} service={params['service']} section={params['section'] ?? (router.current.name === 'service-import-backup' ? 'import-backup' : 'general')} route={router.current.name} stack={params['stack']} backup={params['backup']} />{/key}
    {:else if router.current.name === 'application' || router.current.name === 'application-section' || router.current.name === 'application-backup'}
      {#key router.path}<Application uuid={params.uuid} env={params['env']} app={params['app']} section={params['section'] ?? (params['backup'] ? 'backups' : 'general')} backup={params['backup']} backupSection={params['backupSection']} />{/key}
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
    {:else if router.current.name === 'registries'}
      <DockerRegistries />
    {:else if router.current.name === 'destinations'}
      <Destinations />
    {:else if router.current.name === 'destination'}
      {#key router.path}<Destination uuid={params.uuid} section={params.section ?? 'general'} />{/key}
    {:else if router.current.name === 'sources'}
      <Sources />
    {:else if router.current.name === 'source-github'}
      {#key router.path}<SourceGithub uuid={params.uuid} tab={params.tab ?? 'general'} />{/key}
    {:else if router.current.name === 'source-gitlab' || router.current.name === 'source-gitea'}
      {#key router.path}<SourceOauth provider={router.current.name === 'source-gitlab' ? 'gitlab' : 'gitea'} uuid={params.uuid} />{/key}
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
    {:else if router.current.name === 'terminal'}
      <Terminal />
    {:else if router.current.name === 'settings-general'}
      <SettingsGeneral />
    {:else if router.current.name === 'settings-advanced'}
      <SettingsAdvanced />
    {:else if router.current.name === 'settings-updates'}
      <SettingsUpdates />
    {:else if router.current.name === 'settings-email'}
      <SettingsEmail />
    {:else if router.current.name === 'settings-backup'}
      <SettingsBackup />
    {:else if router.current.name === 'settings-oauth'}
      {#key router.path}<SettingsOauth provider={params.provider} />{/key}
    {:else if router.current.name === 'servers'}
      <Servers />
    {:else if router.current.name === 'server-new' || router.current.name === 'server-new-type'}
      {#key router.path}<ServerNew type={params.type} token={params.token} />{/key}
    {:else if router.current.name === 'server'}
      {#key params.uuid}<Server uuid={params.uuid} />{/key}
    {:else if router.current.name === 'server-section' && params.section === 'danger'}
      {#key params.uuid}<ServerDanger uuid={params.uuid} />{/key}
    {:else if router.current.name === 'server-section'}
      {#key router.path}<ServerSection uuid={params.uuid} section={params.section} sub={params.sub} />{/key}
    {/if}
  </Shell>
{/if}

<Toasts />
