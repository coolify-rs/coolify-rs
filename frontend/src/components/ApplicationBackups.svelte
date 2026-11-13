<script lang="ts">
  // Port of upstream livewire/project/application/backup/index.blade.php (Project/Application/Backup/Index).
  import { untrack } from 'svelte';
  import BackupTargetForm from './BackupTargetForm.svelte';
  import BackupsToolbar from './BackupsToolbar.svelte';
  import Empty from './Empty.svelte';
  import Modal from './Modal.svelte';
  import Reicon from './Reicon.svelte';
  import SettingsSection from './SettingsSection.svelte';
  import StatusBadge from './StatusBadge.svelte';
  import BackupS3Storage from './BackupS3Storage.svelte';
  import SortHeader from './SortHeader.svelte';
  import { backupSortLastRun, backupSortStatus, sortBackups } from '../lib/backupSort';
  import { diffForHumans } from '../lib/format';
  import { link, router } from '../lib/router.svelte';
  import { scheduleStatus, volumeBackupsUi, type BackupsIndex } from '../lib/volumeBackupsApi';

  let { uuid, base }: { uuid: string; base: string } = $props();

  let data = $state<BackupsIndex | null>(null);
  let adding = $state(false);
  // `request()->string('search')`
  let search = $state(new URLSearchParams(location.search).get('search') ?? '');
  let typeFilter = $state('all');
  let sortBy = $state('target_asc');

  async function load() {
    data = await volumeBackupsUi.index('application', uuid);
  }
  $effect(() => {
    untrack(() => load());
    // `BackupCreated` reaches the page over the team channel upstream.
    const poll = setInterval(() => load().catch(() => {}), 10000);
    return () => clearInterval(poll);
  });

  const backups = $derived(data?.backups ?? []);
  const filterOptions = $derived([
    { value: 'all', label: 'All targets' },
    ...[...new Set(backups.map((b) => b.target_type))].map((t) => ({ value: t.toLowerCase(), label: t })),
  ]);
  const filtered = $derived.by(() => {
    const q = search.toLowerCase();
    const list = backups.filter(
      (b) =>
        (!q || b.target_name.toLowerCase().includes(q) || b.target_type.toLowerCase().includes(q) || b.frequency.toLowerCase().includes(q)) &&
        (typeFilter === 'all' || b.target_type.toLowerCase() === typeFilter),
    );
    return sortBackups(list, sortBy, (b) => ({
      name: b.target_name.toLowerCase(),
      type: b.target_type.toLowerCase(),
      frequency: b.frequency.toLowerCase(),
      status: backupSortStatus(b.latest_status, b.enabled),
      s3: b.save_s3 ? b.s3.length : -1,
      lastRunAt: backupSortLastRun(b.latest_finished_at, b.latest_status),
      executions: Number(b.executions_count),
    }));
  });
  const sortTable = (value: string) => (sortBy = value);
  const toggleType = (type: string) => (typeFilter = typeFilter === type.toLowerCase() ? 'all' : type.toLowerCase());
</script>

{#if data}
  <div class="application-settings-form flex min-w-0 flex-col gap-6">
    <SettingsSection title="Storage backups" description="Schedule backups for persistent volumes and directory mounts attached to this application.">
      {#snippet actions()}
        {#if data?.can_update}
          <button type="button" class="button button-highlighted" onclick={() => (adding = true)}>
            <Reicon name="plus" class="size-3.5" />
            Add
          </button>
        {/if}
      {/snippet}
      <div class="grid gap-4 sm:grid-cols-3">
        <div>
          <p class="text-xs font-medium text-neutral-500 dark:text-fg-dim">Schedules</p>
          <p class="mt-1 text-xl font-semibold tabular-nums text-neutral-950 dark:text-fg">{backups.length}</p>
        </div>
        <div>
          <p class="text-xs font-medium text-neutral-500 dark:text-fg-dim">Enabled</p>
          <p class="mt-1 text-xl font-semibold tabular-nums text-neutral-950 dark:text-fg">{backups.filter((b) => b.enabled).length}</p>
        </div>
        <div>
          <p class="text-xs font-medium text-neutral-500 dark:text-fg-dim">Total executions</p>
          <p class="mt-1 text-xl font-semibold tabular-nums text-neutral-950 dark:text-fg">{backups.reduce((n, b) => n + Number(b.executions_count), 0)}</p>
        </div>
      </div>
    </SettingsSection>

    <BackupsToolbar bind:search bind:typeFilter {filterOptions} />

    <SettingsSection title="Schedules" description="Persistent volume and directory backup schedules for this application." flush={backups.length > 0}>
      {#if backups.length > 0 && filtered.length === 0}
        <Empty size="sm" title="No backups found" description="No scheduled backups match your search." />
      {/if}
      {#if backups.length}
        {#if filtered.length}
          <div class="data-table flex w-full flex-col overflow-x-auto">
            <div class="data-table-header backup-table-grid">
              <SortHeader label="Target" asc="target_asc" desc="target_desc" current={sortBy} onsort={sortTable} />
              <SortHeader label="Type" asc="type_asc" desc="type_desc" current={sortBy} onsort={sortTable} />
              <SortHeader label="Schedule" asc="schedule_asc" desc="schedule_desc" current={sortBy} onsort={sortTable} />
              <SortHeader label="Status" asc="status_asc" desc="status_desc" current={sortBy} onsort={sortTable} />
              <SortHeader label="S3" asc="s3_asc" desc="s3_desc" current={sortBy} onsort={sortTable} />
              <SortHeader label="Last run" asc="lastrun_asc" desc="lastrun_desc" first="desc" current={sortBy} onsort={sortTable} />
              <SortHeader label="Executions" asc="executions_asc" desc="executions_desc" first="desc" current={sortBy} onsort={sortTable} class="justify-self-end" />
            </div>
            {#each filtered as b (b.uuid)}
              {@const st = scheduleStatus(b.latest_status, b.enabled)}
              {@const names = b.s3.map((s) => s.name)}
              <a use:link href="{base}/backups/{b.uuid}" class="data-table-row backup-table-grid text-[13px] text-neutral-700 dark:text-fg-dim">
                <span class="min-w-0 truncate font-medium text-neutral-950 dark:text-fg" title={b.target_name}>{b.target_name}</span>
                <span class="min-w-0">
                  <span
                    role="button"
                    tabindex="0"
                    class="table-filter-value"
                    title="Filter by {b.target_type}"
                    onclick={(e) => {
                      e.preventDefault();
                      e.stopPropagation();
                      toggleType(b.target_type);
                    }}
                    onkeydown={(e) => {
                      if (e.key !== 'Enter') return;
                      e.preventDefault();
                      e.stopPropagation();
                      toggleType(b.target_type);
                    }}>{b.target_type}</span
                  >
                </span>
                <span>{b.frequency}</span>
                <span><StatusBadge label={st[0]} type={st[1]} /></span>
                <span class="min-w-0">
                  <BackupS3Storage enabled={b.save_s3} {names} tooltip={b.save_s3 ? (names.length ? `S3 storage: ${names.join(', ')}` : 'S3 storage: Unavailable') : 'S3 storage: Not configured'} />
                </span>
                <span>{b.latest_finished_at ? diffForHumans(b.latest_finished_at) : b.latest_status === 'running' ? 'Running now' : 'Never'}</span>
                <span class="text-right tabular-nums text-neutral-950 dark:text-fg">{b.executions_count}</span>
              </a>
            {/each}
          </div>
        {/if}
      {:else}
        <Empty size="sm" title="No scheduled backups" description="Add a persistent volume or directory backup schedule to protect application data." iconName="storages" />
      {/if}
    </SettingsSection>
  </div>

  {#if adding}
    <Modal title="New scheduled backup" onclose={() => (adding = false)}>
      <BackupTargetForm kind="application" {uuid} targets={data.targets} oncreated={(b) => router.go(`${base}/backups/${b}`)} />
    </Modal>
  {/if}
{/if}
