<script lang="ts">
  // Port of upstream livewire/project/database/backup/index.blade.php (Project/Database/Backup/Index),
  // scheduled-backups.blade.php (Project/Database/ScheduledBackups) and
  // create-scheduled-backup.blade.php (Project/Database/CreateScheduledBackup).
  import { untrack } from 'svelte';
  import Empty from './Empty.svelte';
  import FormInput from './FormInput.svelte';
  import Modal from './Modal.svelte';
  import Reicon from './Reicon.svelte';
  import SettingsSection from './SettingsSection.svelte';
  import StatusBadge from './StatusBadge.svelte';
  import { ApiError, type DatabaseRecord } from '../lib/api';
  import { backupsUi, type BackupSchedule } from '../lib/databaseApi';
  import { link, router } from '../lib/router.svelte';
  import { notify } from '../lib/toast.svelte';

  let { record, base, canUpdate }: { record: DatabaseRecord; base: string; canUpdate: boolean } = $props();

  let schedules = $state<BackupSchedule[]>([]);
  let loaded = $state(false);
  let search = $state('');
  let adding = $state(false);
  let frequency = $state('');
  let error = $state<ApiError | null>(null);

  async function load() {
    const r = await backupsUi.index(record.uuid);
    if (r.unavailable) return router.go(base, true);
    schedules = r.schedules ?? [];
    loaded = true;
  }
  $effect(() => {
    untrack(() => load());
    // `BackupCreated` reaches the page over the team channel upstream.
    const poll = setInterval(() => load().catch(() => {}), 10000);
    return () => clearInterval(poll);
  });

  const enabledCount = $derived(schedules.filter((s) => s.enabled).length);
  const totalExecutions = $derived(schedules.reduce((sum, s) => sum + Number(s.executions_count ?? 0), 0));
  const query = $derived(search.toLowerCase());
  const matches = (s: BackupSchedule) =>
    search === '' ||
    (record.name ?? '').toLowerCase().includes(query) ||
    s.frequency.toLowerCase().includes(query) ||
    s.s3_names.toLowerCase().includes(query);
  const matching = $derived(schedules.filter(matches));

  const latest = (status: string | null): [string, 'success' | 'warning' | 'error' | 'neutral'] => {
    switch (status) {
      case 'success':
        return ['Success', 'success'];
      case 'running':
        return ['In progress', 'warning'];
      case 'failed':
        return ['Failed', 'error'];
      default:
        return ['Never run', 'neutral'];
    }
  };

  async function submit(e: SubmitEvent) {
    e.preventDefault();
    error = null;
    adding = false;
    try {
      const r = await backupsUi.create(record.uuid, frequency);
      frequency = '';
      if (r.backup_uuid) router.go(`${base}/backups/${r.backup_uuid}`);
    } catch (err) {
      error = err instanceof ApiError ? err : new ApiError(0, String(err));
      adding = !!Object.keys(error.errors).length;
      if (!adding) notify.failure(error.message);
    }
  }
</script>

<div class="application-settings-form flex min-w-0 flex-col gap-6">
  <SettingsSection title="Database backups" description="Automate database backups and track the latest execution for each schedule.">
    {#snippet actions()}
      {#if canUpdate}
        <button type="button" class="button button-highlighted" onclick={() => (adding = true)}>+ Add</button>
      {/if}
    {/snippet}
    <div class="grid gap-4 sm:grid-cols-3">
      <div>
        <p class="text-xs font-medium text-neutral-500 dark:text-fg-dim">Schedules</p>
        <p class="mt-1 text-xl font-semibold tabular-nums text-neutral-950 dark:text-fg">{schedules.length}</p>
      </div>
      <div>
        <p class="text-xs font-medium text-neutral-500 dark:text-fg-dim">Enabled</p>
        <p class="mt-1 text-xl font-semibold tabular-nums text-neutral-950 dark:text-fg">{enabledCount}</p>
      </div>
      <div>
        <p class="text-xs font-medium text-neutral-500 dark:text-fg-dim">Total executions</p>
        <p class="mt-1 text-xl font-semibold tabular-nums text-neutral-950 dark:text-fg">{totalExecutions}</p>
      </div>
    </div>
  </SettingsSection>

  {#if loaded}
    <div class="flex min-w-0 flex-col gap-3">
      {#if schedules.length}
        <div class="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
          <div class="relative max-w-sm">
            <Reicon name="search" class="pointer-events-none absolute top-1/2 left-2.5 z-10 size-3.5 -translate-y-1/2 text-neutral-400 dark:text-fg-faint" />
            <input type="search" bind:value={search} aria-label="Search backups" class="input h-8! w-full pl-8! text-[13px]!" placeholder="Search backups" />
          </div>
        </div>
      {/if}

      {#if !schedules.length}
        <Empty size="sm" title="No scheduled backups" description="Create a schedule to start protecting this database." iconName="storages" />
      {:else if matching.length}
        <div class="data-table overflow-hidden rounded-xl border border-neutral-200 bg-white shadow-sm dark:border-white/[0.08] dark:bg-white/[0.03]">
          <div class="data-table-header scheduled-backups-table-grid">
            <span>Schedule</span>
            <span>Latest run</span>
            <span>S3 storage</span>
            <span>Executions</span>
            <span class="text-right">Action</span>
          </div>
          {#each matching as s (s.uuid)}
            {@const st = latest(s.latest_status)}
            <div class="data-table-row scheduled-backups-table-grid border-b border-neutral-200 last:border-b-0 dark:border-white/[0.06]">
              <div class="flex min-w-0 items-center gap-2">
                <span class="block truncate text-[12px] font-semibold text-black dark:text-fg">{s.frequency}</span>
                {#if !s.enabled}
                  <StatusBadge label="Disabled" />
                {/if}
              </div>
              <div class="flex items-center gap-2">
                <StatusBadge label={st[0]} type={st[1]} />
              </div>
              <div class="truncate text-[11px] text-neutral-600 dark:text-fg-dim" title={s.save_s3 ? s.s3_names : ''}>
                {s.save_s3 ? s.s3_names || 'Unavailable' : 'Local only'}
              </div>
              <div class="text-[11px] text-neutral-600 dark:text-fg-dim">
                <a use:link href="{base}/backups/{s.uuid}/executions" class="font-medium hover:underline hover:text-black dark:hover:text-fg">{s.executions_count}</a>
              </div>
              <div class="flex justify-end">
                <a class="button" use:link href="{base}/backups/{s.uuid}">Manage</a>
              </div>
            </div>
          {/each}
          <div class="flex min-h-11 items-center border-t border-neutral-200 px-4 text-[11px] text-neutral-500 dark:border-white/[0.08] dark:text-fg-faint">
            <span>{matching.length} {matching.length === 1 ? 'schedule' : 'schedules'}</span>
          </div>
        </div>
      {:else}
        <div class="border-t border-neutral-200 dark:border-white/[0.06]">
          <Empty size="sm" title="No matching backup schedules" description="Try another database name, frequency, or storage name." />
        </div>
      {/if}
    </div>
  {/if}
</div>

{#if adding}
  <Modal title="New Scheduled Backup" onclose={() => (adding = false)}>
    <form class="application-settings-form flex w-full flex-col gap-4" onsubmit={submit}>
      <FormInput
        placeholder="0 0 * * * or daily"
        id="frequency"
        helper="You can use every_minute, hourly, daily, weekly, monthly, yearly or a cron expression."
        label="Frequency"
        required
        bind:value={frequency}
        error={error?.field('frequency')}
      />
      <div class="flex justify-end border-t border-neutral-200 pt-4 dark:border-white/[0.08]">
        <button type="submit" class="button button-highlighted">Add schedule</button>
      </div>
    </form>
  </Modal>
{/if}
