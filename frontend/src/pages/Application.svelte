<script lang="ts">
  // Port of upstream livewire/project/application/configuration.blade.php (+
  // Project\Application\Configuration), deployment/index.blade.php and
  // deployment/show.blade.php: the heading, the application menu (shown by the
  // main sidebar) and the section. General, Git Source, Deployment Logs and
  // Danger zone are ported; the other sections show the "not available in
  // coolify-rs yet" page in the same frame.
  import ApplicationBackups from '../components/ApplicationBackups.svelte';
  import VolumeBackups from '../components/shared/VolumeBackups.svelte';
  import { volumeBackupsUi } from '../lib/volumeBackupsApi';
  import Empty from '../components/Empty.svelte';
  import ApplicationHeading from '../components/ApplicationHeading.svelte';
  import ApplicationGeneral from '../components/ApplicationGeneral.svelte';
  import ApplicationSource from '../components/ApplicationSource.svelte';
  import DatabaseDanger from '../components/DatabaseDanger.svelte';
  import DeploymentHistory from '../components/DeploymentHistory.svelte';
  import DeploymentLogs from '../components/DeploymentLogs.svelte';
  import EnvironmentVariables from '../components/shared/EnvironmentVariables.svelte';
  import SecretManagerLinks from '../components/shared/SecretManagerLinks.svelte';
  import Storages from '../components/shared/Storages.svelte';
  import HealthChecks from '../components/shared/HealthChecks.svelte';
  import ResourceLimits from '../components/shared/ResourceLimits.svelte';
  import RuntimeLogs from '../components/shared/RuntimeLogs.svelte';
  import ApplicationAdvanced from '../components/shared/ApplicationAdvanced.svelte';
  import ApplicationRollback from '../components/shared/ApplicationRollback.svelte';
  import Metrics from '../components/shared/Metrics.svelte';
  import Tags from '../components/shared/Tags.svelte';
  import Webhooks from '../components/shared/Webhooks.svelte';
  import ResourceOperations from '../components/shared/ResourceOperations.svelte';
  import Destination from '../components/shared/Destination.svelte';
  import FileBrowser from '../components/shared/FileBrowser.svelte';
  import ExecuteContainerCommand from '../components/shared/ExecuteContainerCommand.svelte';
  import ApplicationSwarm from '../components/shared/ApplicationSwarm.svelte';
  import ApplicationDomains from '../components/shared/ApplicationDomains.svelte';
  import Maintenance from '../components/shared/Maintenance.svelte';
  import Unported from './Unported.svelte';
  import { appUi, dbUi, uiApi, ApiError, type ApplicationRecord, type ApplicationState, type DeploymentDetail } from '../lib/api';
  import { applicationBackupNavContext, applicationNavContext, applicationSection } from '../lib/applicationMenu';
  import { setProjectCrumbs } from '../lib/crumbs';
  import { strnatcmp } from '../lib/natsort';
  import { page } from '../lib/page.svelte';
  import { router } from '../lib/router.svelte';
  import { session } from '../lib/session.svelte';

  let {
    uuid,
    env,
    app,
    section,
    dep = null,
    backup = null,
    backupSection = null,
  }: { uuid: string; env: string; app: string; section: string; dep?: string | null; backup?: string | null; backupSection?: string | null } = $props();

  let record = $state<ApplicationRecord | null>(null);
  let appState = $state<ApplicationState | null>(null);
  let resources = $state<{ label: string; href: string; active: boolean }[]>([]);
  let missing = $state(false);
  let shownDep = $state<string | null>(null);
  let detail = $state<DeploymentDetail | null>(null);
  let switching = $state(false);

  const active = $derived(dep ? 'deployment' : applicationSection(section));
  const currentBackupSection = $derived<'general' | 'executions' | 'danger'>(backupSection === 'executions' || backupSection === 'danger' ? backupSection : 'general');
  const base = $derived(`/project/${uuid}/environment/${env}/application/${app}`);
  const canUpdate = $derived(appState?.can_update ?? false);
  const timezone = $derived(appState?.server_timezone ?? 'UTC');
  const serverName = $derived(record?.destination?.server?.name ?? '');

  const limit10 = (s: string) => (s.length > 10 ? `${s.slice(0, 10)}...` : s);

  async function refresh() {
    const [r, s] = await Promise.all([appUi.get(app), appUi.state(app)]);
    record = r;
    appState = s;
  }

  async function load() {
    let r: ApplicationRecord;
    try {
      r = await appUi.get(app);
    } catch (e) {
      if (e instanceof ApiError && (e.status === 404 || e.status === 403)) {
        missing = true;
        return;
      }
      throw e;
    }
    appState = await appUi.state(app);
    if (dep) {
      try {
        detail = await appUi.deployment(app, dep);
        shownDep = dep;
      } catch (e) {
        // Deployment missing: back to the deployments index.
        if (e instanceof ApiError && e.status === 404) {
          router.go(`${base}/deployment`, true);
          return;
        }
        throw e;
      }
    }
    record = r;
    const suffix = dep ? 'Deployment' : active === 'deployment' ? 'Deployments' : active === 'backups' ? 'Backups' : 'Configuration';
    document.title = `${limit10(r.name ?? '')} > ${suffix} | coolify-rs`;
    const bp = r.build_pack ?? '';
    const menu = applicationNavContext(base, active, {
      canAccessTerminal: session.isAdmin,
      // Application::git_based()
      gitBased: !r.dockerfile && bp !== 'dockerimage',
      dockerImage: bp === 'dockerimage',
      compose: bp === 'dockercompose',
      swarm: appState?.is_swarm ?? false,
      // `server-status-badge`.
      serverBadge: r.server_status === false ? 'One or more servers are unreachable or misconfigured.' : undefined,
    });
    page.navContext = backup ? applicationBackupNavContext(base, backup, currentBackupSection, menu) : menu;
    if (backup) {
      // `Route::redirect('/backups/{backup_uuid}/s3')` and `/retention`.
      if (backupSection === 's3' || backupSection === 'retention') return router.go(`${base}/backups/${backup}`, true);
      try {
        await volumeBackupsUi.show('application', app, backup);
      } catch (e) {
        if (e instanceof ApiError && e.status === 404) {
          missing = true;
          return;
        }
        throw e;
      }
    }
    uiApi.project(uuid).then((d) => setProjectCrumbs(d, env));
    dbUi
      .resources(uuid, env)
      .then((res) => {
        const apps = (res.applications ?? []).map((a) => ({
          label: a.name,
          href: `/project/${uuid}/environment/${env}/application/${a.uuid}`,
          active: a.uuid === app,
        }));
        const dbs = res.databases.map((d) => ({ label: d.name, href: `/project/${uuid}/environment/${env}/database/${d.uuid}`, active: false }));
        resources = [...apps, ...dbs].sort((a, b) => strnatcmp(a.label.toLowerCase(), b.label.toLowerCase()));
      })
      .catch(() => {
        resources = record ? [{ label: record.name, href: base, active: true }] : [];
      });
  }

  // `selectDeployment(uuid)`: swap the shown deployment in place.
  async function selectDeployment(next: string) {
    switching = true;
    try {
      detail = await appUi.deployment(app, next);
      shownDep = next;
      history.replaceState(history.state, '', `${base}/deployment/${next}`);
    } finally {
      switching = false;
    }
  }

  $effect(() => {
    load();
    // Heading: `wire:poll.10000ms="checkStatus"`.
    const poll = setInterval(() => record && refresh().catch(() => {}), 10000);
    return () => clearInterval(poll);
  });
</script>

{#if missing}
  <div class="application-settings-form w-full">
    <Empty title="Application not found" description="It may have been deleted, or it belongs to another team." iconName="layers" size="lg" />
  </div>
{:else if record && appState}
  {#if dep && detail && shownDep}
    <div class="flex min-h-[calc(100dvh-7.5rem)] flex-col">
      <ApplicationHeading {record} {appState} {base} {resources} onrefresh={() => refresh().catch(() => {})} />
      <section class="application-settings-workspace mt-4 flex min-h-0 w-full max-w-none flex-1 lg:mt-0">
        <div class="grid min-h-0 min-w-0 flex-1 gap-8 xl:grid-cols-[210px_minmax(0,1fr)] xl:gap-8">
          <aside class="application-settings-navigation in-main-nav min-w-0"></aside>
          <div class="flex min-h-0 min-w-0 flex-col gap-4 xl:h-full xl:pt-px 2xl:grid 2xl:grid-cols-[30rem_minmax(0,1fr)] 2xl:items-start">
            <DeploymentHistory uuid={app} {base} {serverName} {timezone} embedded selected={shownDep} onselect={selectDeployment} />
            <div class={switching ? 'opacity-60' : ''}>
              {#key shownDep}
                <DeploymentLogs
                  uuid={app}
                  dep={shownDep}
                  initial={detail}
                  {canUpdate}
                  {timezone}
                  onfinished={() => refresh().catch(() => {})}
                />
              {/key}
            </div>
          </div>
        </div>
      </section>
    </div>
  {:else if !dep}
    <div>
      <ApplicationHeading {record} {appState} {base} {resources} onrefresh={() => refresh().catch(() => {})} />

      <section class="application-settings-workspace mt-4 w-full max-w-none lg:mt-0">
        <div class="grid min-w-0 gap-8 xl:grid-cols-[210px_minmax(0,1fr)] xl:gap-8">
          <aside class="application-settings-navigation in-main-nav min-w-0"></aside>
          <div class="min-w-0">
            {#if active === 'general'}
              {#if ['dockerimage', 'dockerfile', 'static', 'nixpacks', 'railpack'].includes(record.build_pack ?? '')}
                <ApplicationGeneral {record} {appState} {base} onsaved={() => refresh().catch(() => {})} />
              {:else}
                <Unported description="The settings of this build pack are not ported to coolify-rs yet." />
              {/if}
            {:else if active === 'source' && !record.dockerfile && record.build_pack !== 'dockerimage'}
              <ApplicationSource uuid={app} onsaved={() => refresh().catch(() => {})} />
            {:else if active === 'domains'}
              <ApplicationDomains uuid={app} {base} onchange={() => refresh().catch(() => {})} />
              <div class="mt-4"><Maintenance kind="application" uuid={app} /></div>
            {:else if active === 'environment-variables'}
              <EnvironmentVariables kind="application" uuid={app} onconfigurationchanged={() => refresh().catch(() => {})} />
              <SecretManagerLinks kind="application" uuid={app} />
            {:else if active === 'backups' && backup}
              {#key `${backup}-${currentBackupSection}`}
                <VolumeBackups
                  kind="application"
                  uuid={app}
                  backupUuid={backup}
                  section={currentBackupSection}
                  actionsInHeader
                  {canUpdate}
                  onexecutions={() => router.go(`${base}/backups/${backup}/executions`)}
                  ondeleted={() => router.go(`${base}/backups`)}
                />
              {/key}
            {:else if active === 'backups'}
              <ApplicationBackups uuid={app} {base} />
            {:else if active === 'persistent-storage'}
              <Storages kind="application" uuid={app} onchange={() => refresh().catch(() => {})} />
            {:else if active === 'logs'}
              <div class="runtime-logs-column min-w-0"><RuntimeLogs kind="application" uuid={app} /></div>
            {:else if active === 'advanced'}
              <ApplicationAdvanced uuid={app} {base} onchange={() => refresh().catch(() => {})} />
            {:else if active === 'rollback'}
              <ApplicationRollback uuid={app} {base} />
            {:else if active === 'swarm' && appState.is_swarm}
              <ApplicationSwarm uuid={app} onchange={() => refresh().catch(() => {})} />
            {:else if active === 'metrics'}
              <Metrics kind="application" uuid={app} />
            {:else if active === 'webhooks'}
              <Webhooks kind="application" uuid={app} />
            {:else if active === 'healthcheck' && record.build_pack !== 'dockercompose'}
              <HealthChecks kind="application" uuid={app} onchange={() => refresh().catch(() => {})} />
            {:else if active === 'resource-limits'}
              <ResourceLimits kind="application" uuid={app} />
            {:else if active === 'resource-operations'}
              <ResourceOperations kind="application" uuid={app} />
            {:else if active === 'files'}
              <FileBrowser kind="application" uuid={app} />
            {:else if active === 'terminal'}
              <ExecuteContainerCommand kind="application" uuid={app} />
            {:else if active === 'servers'}
              <Destination kind="application" uuid={app} {base} onchange={() => refresh().catch(() => {})} />
            {:else if active === 'tags'}
              <Tags kind="application" uuid={app} />
            {:else if active === 'deployment'}
              <DeploymentHistory uuid={app} {base} {serverName} {timezone} />
            {:else if active === 'danger'}
              <DatabaseDanger kind="application" {record} canDelete={canUpdate} back="/project/{uuid}/environment/{env}" />
            {:else}
              <Unported />
            {/if}
          </div>
        </div>
      </section>
    </div>
  {/if}
{/if}
