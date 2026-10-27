<script lang="ts">
  // Port of upstream livewire/project/database/configuration.blade.php (+
  // Project\Database\Configuration): the heading, the database
  // menu (shown by the main sidebar) and the section. General and Danger zone
  // are ported; the other sections and the Backups / Runtime Logs / Terminal /
  // Files pages show the "not available in coolify-rs yet" page in the same frame.
  import Empty from '../components/Empty.svelte';
  import DatabaseHeading from '../components/DatabaseHeading.svelte';
  import DatabaseGeneral from '../components/DatabaseGeneral.svelte';
  import DatabaseDanger from '../components/DatabaseDanger.svelte';
  import Unported from './Unported.svelte';
  import { dbUi, uiApi, ApiError, type DatabaseRecord, type DatabaseState } from '../lib/api';
  import { databaseNavContext, databaseSection } from '../lib/databaseMenu';
  import { setProjectCrumbs } from '../lib/crumbs';
  import { strnatcmp } from '../lib/natsort';
  import { page } from '../lib/page.svelte';
  import { session } from '../lib/session.svelte';

  let { uuid, env, db, section }: { uuid: string; env: string; db: string; section: string } = $props();

  let record = $state<DatabaseRecord | null>(null);
  let dbState = $state<DatabaseState | null>(null);
  let resources = $state<{ label: string; href: string; active: boolean }[]>([]);
  let missing = $state(false);

  const active = $derived(databaseSection(section));
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
    document.title = `${r.name.slice(0, 10)} > Configuration | coolify-rs`;
    page.navContext = databaseNavContext(base, active, {
      canUpdate: dbState.can_update,
      canAccessTerminal: session.isAdmin,
      // `isBackupSolutionAvailable()`: not for Redis, KeyDB and Dragonfly.
      backups: !['redis', 'keydb', 'dragonfly'].includes(dbState.kind ?? ''),
      initScripts: (dbState.kind ?? 'postgresql') === 'postgresql',
    });
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
</script>

{#if missing}
  <div class="application-settings-form w-full">
    <Empty title="Database not found" description="It may have been deleted, or it belongs to another team." iconName="database" size="lg" />
  </div>
{:else if record && dbState}
  <div>
    <DatabaseHeading {record} {dbState} healthcheckUrl="{base}/healthcheck" {resources} onrefresh={() => refresh().catch(() => {})} />

    <section class="application-settings-workspace mt-4 w-full max-w-none lg:mt-0">
      <div class="grid min-w-0 gap-8 xl:grid-cols-[210px_minmax(0,1fr)] xl:gap-8">
        <aside class="application-settings-navigation in-main-nav min-w-0"></aside>
        <div class="min-w-0">
          {#if active === 'general'}
            {#if kind === '' || kind === 'sqlite'}
              <Unported description="The settings of this database type are not ported to coolify-rs yet." />
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
              <DatabaseDanger {record} canDelete={canUpdate} back="/project/{uuid}/environment/{env}" />
            {/if}
          {:else}
            <Unported />
          {/if}
        </div>
      </div>
    </section>
  </div>
{/if}
