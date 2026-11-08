<script lang="ts">
  // Port of upstream livewire/project/service/volume-backup/index.blade.php (Project/Service/VolumeBackup/Index).
  import { untrack } from 'svelte';
  import BackupTargetForm from './BackupTargetForm.svelte';
  import BackupsToolbar from './BackupsToolbar.svelte';
  import SortHeader from './SortHeader.svelte';
  import RowActions from './RowActions.svelte';
  import RowAction from './RowAction.svelte';
  import { backupSortLastRun, backupSortStatus, sortBackups } from '../lib/backupSort';
  import DatabaseBackupEdit from './DatabaseBackupEdit.svelte';
  import Empty from './Empty.svelte';
  import FormInput from './FormInput.svelte';
  import Listbox from './Listbox.svelte';
  import Modal from './Modal.svelte';
  import Reicon from './Reicon.svelte';
  import ServiceBackupExecutions from './ServiceBackupExecutions.svelte';
  import SettingsSection from './SettingsSection.svelte';
  import StatusBadge from './StatusBadge.svelte';
  import VolumeBackups from './shared/VolumeBackups.svelte';
  import { ApiError, type DatabaseRecord } from '../lib/api';
  import { backupsUi, type BackupEdit } from '../lib/databaseApi';
  import { diffForHumans } from '../lib/format';
  import { router } from '../lib/router.svelte';
  import { notify } from '../lib/toast.svelte';
  import { scheduleStatus, volumeBackupsUi, type BackupsIndex, type S3Label, type ServiceDatabaseSchedule, type VolumeSchedule } from '../lib/volumeBackupsApi';

  let { service, base }: { service: string; base: string } = $props();

  type Row = {
    id: string;
    uuid: string;
    kind: 'database' | 'storage';
    name: string;
    type: string;
    frequency: string;
    enabled: boolean;
    save_s3: boolean;
    s3: S3Label[];
    latest_status: string | null;
    latest_finished_at: string | null;
    count: number;
    created_at: string;
    database?: ServiceDatabaseSchedule;
    volume?: VolumeSchedule;
  };

  let data = $state<BackupsIndex | null>(null);
  const query = new URLSearchParams(location.search);
  let search = $state(query.get('search') ?? '');
  let typeFilter = $state('all');
  let sortBy = $state('target_asc');
  let menu = $state(false);
  let addStorage = $state(false);
  let addDatabase = $state(false);
  let selectedDatabase = $state<string>('');
  let databaseFrequency = $state('');
  let databaseError = $state<ApiError | null>(null);
  let opened = $state<Row | null>(null);
  let openedEdit = $state<BackupEdit | null>(null);
  let modalSection = $state<'general' | 'danger'>('general');

  async function load() {
    data = await volumeBackupsUi.index('service', service);
  }
  /** `openSchedule`, also from `#[Url(as: 'backup_uuid')]`. */
  async function openSchedule(uuid: string) {
    const row = rows.find((r) => r.uuid === uuid);
    if (!row) return;
    modalSection = 'general';
    openedEdit = row.database ? await backupsUi.edit(row.database.database_uuid, uuid) : null;
    opened = row;
    const url = new URL(location.href);
    url.searchParams.set('backup_uuid', uuid);
    history.replaceState(history.state, '', url);
  }
  /** `closeScheduleModal`. */
  function closeSchedule() {
    opened = null;
    openedEdit = null;
    const url = new URL(location.href);
    url.searchParams.delete('backup_uuid');
    history.replaceState(history.state, '', url);
    load().catch(() => {});
  }
  $effect(() => {
    untrack(async () => {
      await load();
      const wanted = query.get('backup_uuid');
      if (wanted && data?.can_update) await openSchedule(wanted);
    });
    // `ServiceChecked`, `ServiceStatusChanged` and `BackupCreated` reach the page over the team channel upstream.
    const poll = setInterval(() => load().catch(() => {}), 10000);
    return () => clearInterval(poll);
  });

  const rows = $derived<Row[]>([
    ...(data?.database_backups ?? []).map(
      (b): Row => ({
        id: `database:${b.id}`,
        uuid: b.uuid,
        kind: 'database',
        name: b.target_name,
        type: 'Database',
        frequency: b.frequency,
        enabled: b.enabled,
        save_s3: b.save_s3,
        s3: b.s3,
        latest_status: b.latest_status,
        latest_finished_at: b.latest_finished_at,
        count: Number(b.executions_count),
        created_at: b.created_at,
        database: b,
      }),
    ),
    ...(data?.backups ?? []).map(
      (b): Row => ({
        id: `storage:${b.id}`,
        uuid: b.uuid,
        kind: 'storage',
        name: b.target_name,
        type: b.target_type,
        frequency: b.frequency,
        enabled: b.enabled,
        save_s3: b.save_s3,
        s3: b.s3,
        latest_status: b.latest_status,
        latest_finished_at: b.latest_finished_at,
        count: Number(b.executions_count),
        created_at: b.created_at,
        volume: b,
      }),
    ),
  ]);
  const filterOptions = $derived([
    { value: 'all', label: 'All targets' },
    ...[...new Set([...(data?.backups ?? []).map((b) => b.target_type), 'Database'])].map((t) => ({ value: t.toLowerCase(), label: t })),
  ]);
  const filtered = $derived.by(() => {
    const q = search.toLowerCase();
    const list = rows.filter(
      (b) => (!q || b.name.toLowerCase().includes(q) || b.type.toLowerCase().includes(q) || b.frequency.toLowerCase().includes(q)) && (typeFilter === 'all' || b.type.toLowerCase() === typeFilter),
    );
    return sortBackups(list, sortBy, (b) => ({
      name: b.name.toLowerCase(),
      type: b.type.toLowerCase(),
      frequency: b.frequency.toLowerCase(),
      status: backupSortStatus(b.latest_status, b.enabled),
      s3: b.save_s3 ? s3Of(b).length : -1,
      lastRunAt: backupSortLastRun(b.latest_finished_at, b.latest_status),
      executions: b.count,
    }));
  });
  const sortTable = (value: string) => (sortBy = value);
  const toggleType = (type: string) => (typeFilter = typeFilter === type.toLowerCase() ? 'all' : type.toLowerCase());

  const s3Of = (r: Row) => (data ? r.s3.filter((s) => s.team_id === data?.team_id) : []);
  const s3Tooltip = (r: Row) => {
    const list = s3Of(r);
    return !r.save_s3 ? 'S3 storage: Not configured' : list.length ? `S3 storage: ${list.map((s) => `${s.name} (bucket: ${s.bucket})`).join(', ')}` : 'S3 storage: Unavailable';
  };
  const databaseRunning = (r: Row) => (r.database?.database_status ?? '').startsWith('running');
  const databaseRecord = (r: Row) => ({ uuid: r.database?.database_uuid, name: r.name, status: r.database?.database_status ?? '' }) as DatabaseRecord;

  async function backupNow(r: Row) {
    await volumeBackupsUi.backupNow(service, r.kind, r.uuid);
    await load();
  }

  function openDatabaseForm() {
    menu = false;
    selectedDatabase = data?.database_targets[0]?.uuid ?? '';
    databaseFrequency = '';
    databaseError = null;
    addDatabase = true;
  }

  /** `CreateScheduledBackup::submit`. */
  async function createDatabaseBackup(e: Event) {
    e.preventDefault();
    databaseError = null;
    try {
      const r = await backupsUi.create(selectedDatabase, databaseFrequency);
      if (r.backup_uuid) {
        addDatabase = false;
        router.go(`${base}/storage-backups?backup_uuid=${r.backup_uuid}`);
      }
    } catch (err) {
      if (err instanceof ApiError && err.status === 422) databaseError = err;
      else notify.failure(err instanceof ApiError ? err.message : String(err));
    } finally {
      databaseFrequency = '';
    }
  }
</script>

<svelte:window onclick={() => (menu = false)} />

{#if data}
  {#if opened}
    {@const row = opened}
    <Modal title="Edit backup schedule" isLarge fixedHeight focusFirstField={false} onclose={closeSchedule}>
      <div class="flex min-w-0 flex-col gap-6">
        <div>
          <h2 class="text-base font-semibold text-neutral-950 dark:text-fg">{row.name}</h2>
          <p class="mt-1 text-xs text-neutral-500 dark:text-fg-dim">{row.frequency} schedule</p>
        </div>
        <nav aria-label="Backup sections" class="flex min-w-0 flex-wrap gap-1 border-b border-neutral-200 pb-2 dark:border-white/[0.08]">
          {#each [['general', 'General'], ['danger', 'Danger Zone']] as [key, label] (key)}
            <button
              type="button"
              onclick={() => (modalSection = key as 'general' | 'danger')}
              class="{modalSection === key
                ? 'bg-coollabs/10 text-coollabs ring-1 ring-coollabs/25 dark:bg-brand/15 dark:text-brand-light dark:ring-brand-light/25'
                : 'text-neutral-500 hover:bg-black/5 hover:text-neutral-900 dark:text-fg-dim dark:hover:bg-white/[0.06] dark:hover:text-fg'} inline-flex h-8 shrink-0 cursor-pointer items-center rounded-md px-3 text-[13px] font-medium transition-colors"
            >
              {label}
            </button>
          {/each}
        </nav>
        {#if row.database && openedEdit}
          {#key modalSection}
            <DatabaseBackupEdit
              record={databaseRecord(row)}
              backupUuid={row.uuid}
              section={modalSection}
              base="{base}/{row.database.database_uuid}"
              edit={openedEdit}
              canUpdate={data.can_update}
              actionsInHeader={false}
              onchanged={(e) => (openedEdit = e)}
            />
          {/key}
        {:else}
          {#key modalSection}
            <VolumeBackups
              kind="service"
              uuid={service}
              backupUuid={row.uuid}
              section={modalSection}
              onexecutions={closeSchedule}
              ondeleted={closeSchedule}
              canUpdate={data.can_update}
            />
          {/key}
        {/if}
      </div>
    </Modal>
  {/if}

  <div class="application-settings-form min-w-0 flex flex-col gap-6">
    <SettingsSection title="Backups" description="Manage database, persistent volume, and directory backup schedules for this service.">
      {#snippet actions()}
        {#if data?.can_update}
          <div>
            <div class="relative">
              <button
                type="button"
                class="button button-highlighted"
                aria-haspopup="menu"
                aria-expanded={menu}
                onclick={(e) => {
                  e.stopPropagation();
                  menu = !menu;
                }}
              >
                <Reicon name="plus" class="size-3.5" />
                Add backup
                <Reicon name="chevron-down" class="size-3 opacity-55" />
              </button>
              {#if menu}
                <div role="menu" class="listbox-panel left-0! right-auto! z-[90]! w-52! min-w-52! sm:left-auto! sm:right-0!">
                  <button
                    type="button"
                    role="menuitem"
                    class="listbox-option justify-start! gap-2.5!"
                    onclick={() => {
                      menu = false;
                      addStorage = true;
                    }}
                  >
                    <Reicon name="storages" class="size-3.5" />
                    Storage backup
                  </button>
                  {#if data?.database_targets.length}
                    <button type="button" role="menuitem" class="listbox-option justify-start! gap-2.5!" onclick={openDatabaseForm}>
                      <Reicon name="database" class="size-3.5" />
                      Database backup
                    </button>
                  {/if}
                </div>
              {/if}
            </div>
          </div>
        {/if}
      {/snippet}
      <div class="grid gap-4 sm:grid-cols-3">
        <div>
          <p class="text-xs font-medium text-neutral-500 dark:text-fg-dim">Schedules</p>
          <p class="mt-1 text-xl font-semibold tabular-nums text-neutral-950 dark:text-fg">{rows.length}</p>
        </div>
        <div>
          <p class="text-xs font-medium text-neutral-500 dark:text-fg-dim">Enabled</p>
          <p class="mt-1 text-xl font-semibold tabular-nums text-neutral-950 dark:text-fg">{rows.filter((r) => r.enabled).length}</p>
        </div>
        <div>
          <p class="text-xs font-medium text-neutral-500 dark:text-fg-dim">Total executions</p>
          <p class="mt-1 text-xl font-semibold tabular-nums text-neutral-950 dark:text-fg">{rows.reduce((n, r) => n + r.count, 0)}</p>
        </div>
      </div>
    </SettingsSection>

    <BackupsToolbar bind:search bind:typeFilter {filterOptions} />

    <SettingsSection title="Schedules" description="Database, persistent volume, and directory backup schedules for this service." flush={rows.length > 0}>
      {#if rows.length > 0 && filtered.length === 0}
        <Empty size="sm" title="No backups found" description="No scheduled backups match your search." />
      {/if}
      {#if rows.length}
        {#if filtered.length}
          <div class="data-table w-full overflow-x-auto">
            <div class="flex min-w-[64rem] flex-col">
              <div class="data-table-header backup-table-grid service-backup-table-grid">
                <SortHeader label="Target" asc="target_asc" desc="target_desc" current={sortBy} onsort={sortTable} />
                <SortHeader label="Type" asc="type_asc" desc="type_desc" current={sortBy} onsort={sortTable} />
                <SortHeader label="Schedule" asc="schedule_asc" desc="schedule_desc" current={sortBy} onsort={sortTable} />
                <SortHeader label="Status" asc="status_asc" desc="status_desc" current={sortBy} onsort={sortTable} />
                <SortHeader label="S3" asc="s3_asc" desc="s3_desc" current={sortBy} onsort={sortTable} />
                <SortHeader label="Last run" asc="lastrun_asc" desc="lastrun_desc" first="desc" current={sortBy} onsort={sortTable} />
                <span class="text-right">Actions</span>
              </div>
              {#each filtered as r (r.id)}
                {@const st = scheduleStatus(r.latest_status, r.enabled)}
                {@const s3 = s3Of(r)}
                <div
                  onclick={() => openSchedule(r.uuid)}
                  onkeydown={(e) => e.key === 'Enter' && openSchedule(r.uuid)}
                  role="button"
                  tabindex="0"
                  class="data-table-row backup-table-grid cursor-pointer text-left text-[13px] text-neutral-700 service-backup-table-grid dark:text-fg-dim"
                >
                  <span class="min-w-0 truncate font-medium text-neutral-950 dark:text-fg" title={r.kind === 'storage' ? r.name : undefined}>{r.name}</span>
                  <span class="min-w-0">
                    <button
                      type="button"
                      class="table-filter-value"
                      title="Filter by {r.type}"
                      onclick={(e) => {
                        e.stopPropagation();
                        toggleType(r.type);
                      }}
                      onkeydown={(e) => e.key === 'Enter' && e.stopPropagation()}>{r.type}</button
                    >
                  </span>
                  <span>{r.frequency}</span>
                  <span><StatusBadge label={st[0]} type={st[1]} /></span>
                  <span>
                    <span role="note" data-tooltip={s3Tooltip(r)} aria-label={s3Tooltip(r)}>
                      <StatusBadge
                        label={r.save_s3 ? (s3.length === 0 ? 'Unavailable' : s3.length === 1 ? 'Configured' : `${s3.length} storages`) : 'Not set'}
                        type={r.save_s3 ? (s3.length ? 'success' : 'error') : 'neutral'}
                      />
                    </span>
                  </span>
                  <span>{r.latest_finished_at ? diffForHumans(r.latest_finished_at) : r.latest_status === 'running' ? 'Running now' : 'Never'}</span>
                  <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
                  <span class="flex justify-end" onclick={(e) => e.stopPropagation()} onkeydown={(e) => e.key === 'Enter' && e.stopPropagation()}>
                    {#if data.can_update}
                      <RowActions label="Backup schedule actions">
                        {#if r.kind === 'database'}
                          <RowAction
                            disabled={!databaseRunning(r)}
                            title={databaseRunning(r) ? undefined : 'The database must be running to start a backup.'}
                            onclick={() => backupNow(r)}>Back up now</RowAction
                          >
                        {:else}
                          <RowAction onclick={() => backupNow(r)}>Back up now</RowAction>
                        {/if}
                        <RowAction onclick={() => openSchedule(r.uuid)}>Edit</RowAction>
                      </RowActions>
                    {/if}
                  </span>
                </div>
              {/each}
            </div>
          </div>
        {/if}
      {:else}
        <Empty size="sm" title="No scheduled backups" description="Add a database, persistent volume, or directory backup schedule to protect service data." iconName="storages" />
      {/if}
    </SettingsSection>

    <ServiceBackupExecutions {service} />
  </div>

  {#if addStorage}
    <Modal title="New storage backup" onclose={() => (addStorage = false)}>
      <BackupTargetForm
        kind="service"
        uuid={service}
        targets={data.targets}
        oncreated={(b) => {
          addStorage = false;
          // Create redirects to `volume-backups.show`, which redirects to the index.
          router.go(`${base}/storage-backups/${b}`);
        }}
      />
    </Modal>
  {/if}

  {#if addDatabase}
    <Modal title="New database backup" onclose={() => (addDatabase = false)}>
      <form class="application-settings-form flex w-full flex-col gap-4" onsubmit={createDatabaseBackup}>
        <Listbox
          id="selectedDatabaseUuid"
          label="Database"
          required
          emptyText="No databases in this service support scheduled backups."
          bind:value={selectedDatabase}
          options={data.database_targets.map((d) => ({ value: d.uuid, label: d.name }))}
        />
        <FormInput
          placeholder="0 0 * * * or daily"
          id="frequency"
          helper="You can use every_minute, hourly, daily, weekly, monthly, yearly or a cron expression."
          label="Frequency"
          required
          bind:value={databaseFrequency}
          error={databaseError?.field('frequency')}
        />
        <div class="flex justify-end border-t border-neutral-200 pt-4 dark:border-white/[0.08]">
          <button type="submit" class="button button-highlighted">Add schedule</button>
        </div>
      </form>
    </Modal>
  {/if}
{/if}
