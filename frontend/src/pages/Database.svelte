<script lang="ts">
  // Port of upstream livewire/project/database/configuration.blade.php (+
  // Project\Database\Configuration): the heading, the database menu (shown by the
  // main sidebar) and the section; backup/index.blade.php and backup/execution.blade.php
  // (Project/Database/Backup/Execution) for the backup pages. The `Project/Shared/*`
  // sections render a plain placeholder until the shared components are connected.
  import Empty from '../components/Empty.svelte';
  import Forbidden from '../components/Forbidden.svelte';
  import DatabaseHeading from '../components/DatabaseHeading.svelte';
  import DatabaseGeneral from '../components/DatabaseGeneral.svelte';
  import DatabaseDanger from '../components/DatabaseDanger.svelte';
  import DatabaseSqliteGeneral from '../components/DatabaseSqliteGeneral.svelte';
  import DatabaseHealth from '../components/DatabaseHealth.svelte';
  import DatabaseImport from '../components/DatabaseImport.svelte';
  import DatabaseBackups from '../components/DatabaseBackups.svelte';
  import DatabaseBackupEdit from '../components/DatabaseBackupEdit.svelte';
  import DatabaseBackupExecutions from '../components/DatabaseBackupExecutions.svelte';
  import EnvironmentVariables from '../components/shared/EnvironmentVariables.svelte';
  import Metrics from '../components/shared/Metrics.svelte';
  import ResourceLimits from '../components/shared/ResourceLimits.svelte';
  import RuntimeLogs from '../components/shared/RuntimeLogs.svelte';
  import SecretManagerLinks from '../components/shared/SecretManagerLinks.svelte';
  import Storages from '../components/shared/Storages.svelte';
  import Tags from '../components/shared/Tags.svelte';
  import Webhooks from '../components/shared/Webhooks.svelte';
  import ResourceOperations from '../components/shared/ResourceOperations.svelte';
  import Destination from '../components/shared/Destination.svelte';
  import FileBrowser from '../components/shared/FileBrowser.svelte';
  import ExecuteContainerCommand from '../components/shared/ExecuteContainerCommand.svelte';
  import Unported from './Unported.svelte';
  import { dbUi, uiApi, ApiError, type DatabaseRecord, type DatabaseState } from '../lib/api';
  import { backupNavContext, databaseNavContext, databaseSection, teleport } from '../lib/databaseMenu';
  import { backupsUi, type BackupEdit } from '../lib/databaseApi';
  import { router } from '../lib/router.svelte';
  import { setProjectCrumbs } from '../lib/crumbs';
  import { strnatcmp } from '../lib/natsort';
  import { page } from '../lib/page.svelte';
  import { session } from '../lib/session.svelte';

  let {
    uuid,
    env,
    db,
    section,
    backup,
    backupSection,
  }: { uuid: string; env: string; db: string; section: string; backup?: string; backupSection?: string } = $props();

  let record = $state<DatabaseRecord | null>(null);
  let dbState = $state<DatabaseState | null>(null);
  let resources = $state<{ label: string; href: string; active: boolean }[]>([]);
  let missing = $state(false);
  let backupEdit = $state<BackupEdit | null>(null);

  const active = $derived(databaseSection(section));
  const currentBackupSection = $derived(backupSection === 'executions' || backupSection === 'danger' ? backupSection : 'general');
  const base = $derived(`/project/${uuid}/environment/${env}/database/${db}`);
  const canUpdate = $derived(dbState?.can_update ?? false);
  const kind = $derived(dbState?.kind ?? '');

  async function loadState(): Promise<DatabaseState> {
    try {
      return await dbUi.state(db);
    } catch (e) {
      if (e instanceof ApiError && e.status === 404) {
        // The state endpoint is optional for rendering; permissions follow the team role then.
        return { activity: null, configuration_changed: false, can_update: session.isAdmin };
      }
      throw e;
    }
  }

  async function refresh() {
    const [r, s] = await Promise.all([dbUi.get(db), loadState()]);
    record = r;
    dbState = s;
  }

  async function load() {
    let r: DatabaseRecord;
    try {
      r = await dbUi.get(db);
    } catch (e) {
      if (e instanceof ApiError && (e.status === 404 || e.status === 403)) {
        missing = true;
        return;
      }
      throw e;
    }
    dbState = await loadState();
    record = r;
    document.title = `${r.name.slice(0, 10)} > ${backup ? 'Backup' : active === 'backups' ? 'Backups' : 'Configuration'} | coolify-rs`;
    const menu = databaseNavContext(base, active, {
      canUpdate: dbState.can_update,
      canAccessTerminal: session.isAdmin,
      // `isBackupSolutionAvailable()`: not for Redis, KeyDB and Dragonfly.
      backups: !['redis', 'keydb', 'dragonfly'].includes(dbState.kind ?? ''),
      initScripts: (dbState.kind ?? 'postgresql') === 'postgresql',
      sqlite: dbState.kind === 'sqlite',
    });
    page.navContext = menu;
    if (backup) {
      // `/backups/{uuid}/s3` and `/retention` redirect to the backup's General page.
      if (backupSection === 's3' || backupSection === 'retention') return router.go(`${base}/backups/${backup}`, true);
      page.navContext = backupNavContext(base, backup, currentBackupSection, menu);
      try {
        backupEdit = await backupsUi.edit(db, backup);
      } catch (e) {
        if (e instanceof ApiError && e.status === 404) return router.go('/', true);
        throw e;
      }
    }
    uiApi.project(uuid).then((d) => setProjectCrumbs(d, env));
    dbUi
      .resources(uuid, env)
      .then((r) => {
        resources = r.databases
          .map((d) => ({ label: d.name, href: `/project/${uuid}/environment/${env}/database/${d.uuid}`, active: d.uuid === db }))
          .sort((a, b) => strnatcmp(a.label.toLowerCase(), b.label.toLowerCase()));
      })
      .catch(() => {
        resources = record ? [{ label: record.name, href: base, active: true }] : [];
      });
  }

  $effect(() => {
    load();
    // Heading: `wire:poll.10000ms="checkStatus"`.
    const poll = setInterval(() => record && refresh().catch(() => {}), 10000);
    return () => clearInterval(poll);
  });

  const running = $derived((record?.status ?? '').startsWith('running'));
  const backupEnabled = $derived(!!backupEdit?.form.enabled);

  // `ManagesDatabaseBackup` for the header actions of `Backup\Execution`.
  async function headerToggle() {
    if (!backup) return;
    const r = await backupsUi.action(db, backup, 'toggleEnabled');
    if (r.backup) backupEdit = r.backup;
  }
  async function headerBackupNow() {
    if (!backup) return;
    const r = await backupsUi.action(db, backup, 'backupNow');
    if (r.redirect_executions) router.go(`${base}/backups/${backup}/executions`);
  }
</script>

{#if missing}
  <div class="application-settings-form w-full">
    <Empty title="Database not found" description="It may have been deleted, or it belongs to another team." iconName="database" size="lg" />
  </div>
{:else if record && dbState && active === 'import-backup' && !canUpdate}
  <!-- `->middleware('can.update.resource')` of the import-backup route -->
  <Forbidden />
{:else if record && dbState}
  <div>
    <DatabaseHeading {record} {dbState} healthcheckUrl="{base}/healthcheck" {resources} onrefresh={() => refresh().catch(() => {})} />

    {#if backup && canUpdate && backupEdit}
      <!-- @teleport('#resource-action-hud-slot') of backup/execution.blade.php -->
      <div class="order-first mr-1 hidden items-center gap-1 xl:flex" use:teleport={'resource-action-hud-slot'}>
        {#if backupEnabled}
          <button type="button" class="button" onclick={headerToggle}>Disable backup</button>
        {:else}
          <button type="button" class="button button-highlighted" onclick={headerToggle}>Enable backup</button>
        {/if}
        <button type="button" class="button" disabled={!running} title={running ? undefined : 'The database must be running to start a backup.'} onclick={headerBackupNow}>Back up now</button>
      </div>
    {/if}

    <section class="application-settings-workspace mt-4 w-full max-w-none lg:mt-0">
      <div class="grid min-w-0 gap-8 xl:grid-cols-[210px_minmax(0,1fr)] xl:gap-8">
        <aside class="application-settings-navigation in-main-nav min-w-0"></aside>
        <div class="min-w-0">
          {#if backup}
            {#if currentBackupSection === 'executions'}
              <DatabaseBackupExecutions databaseUuid={db} backupUuid={backup} />
            {:else if backupEdit}
              <DatabaseBackupEdit {record} backupUuid={backup} section={currentBackupSection} {base} edit={backupEdit} {canUpdate} onchanged={(e) => (backupEdit = e)} />
            {/if}
          {:else if active === 'backups'}
            <DatabaseBackups {record} {base} {canUpdate} />
          {:else if active === 'import-backup'}
            <DatabaseImport {record} {canUpdate} />
          {:else if active === 'healthcheck'}
            <DatabaseHealth {record} {canUpdate} onsaved={() => refresh().catch(() => {})} />
          {:else if active === 'general'}
            {#if kind === ''}
              <Unported description="The settings of this database type are not ported to coolify-rs yet." />
            {:else if kind === 'sqlite'}
              <DatabaseSqliteGeneral {record} {canUpdate} onsaved={() => refresh().catch(() => {})} />
            {:else if dbState}
              <DatabaseGeneral
                {record}
                {kind}
                extras={dbState.form ?? { credential_locked: false }}
                {canUpdate}
                onsaved={() => refresh().catch(() => {})}
              />
            {/if}
          {:else if active === 'danger'}
            {#if dbState}
              <DatabaseDanger {record} canDelete={canUpdate} flushable={dbState?.flushable === true} back="/project/{uuid}/environment/{env}" />
            {/if}
          {:else if active === 'environment-variables'}
            <EnvironmentVariables kind="database" uuid={db} onconfigurationchanged={() => refresh().catch(() => {})} />
            <SecretManagerLinks kind="database" uuid={db} />
          {:else if active === 'persistent-storage'}
            <Storages kind="database" uuid={db} onchange={() => refresh().catch(() => {})} />
          {:else if active === 'webhooks'}
            <Webhooks kind="database" uuid={db} />
          {:else if active === 'resource-limits'}
            <ResourceLimits kind="database" uuid={db} />
          {:else if active === 'metrics'}
            <Metrics kind="database" uuid={db} />
          {:else if active === 'files'}
            <FileBrowser kind="database" uuid={db} />
          {:else if active === 'servers'}
            <Destination kind="database" uuid={db} />
          {:else if active === 'resource-operations'}
            <ResourceOperations kind="database" uuid={db} />
          {:else if active === 'tags'}
            <Tags kind="database" uuid={db} />
          {:else if active === 'logs'}
            <div class="runtime-logs-column min-w-0"><RuntimeLogs kind="database" uuid={db} /></div>
          {:else if active === 'terminal'}
            <ExecuteContainerCommand kind="database" uuid={db} />
          {:else}
            <Unported />
          {/if}
        </div>
      </div>
    </section>
  </div>
{/if}
