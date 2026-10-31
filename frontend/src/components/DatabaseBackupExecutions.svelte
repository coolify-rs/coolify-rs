<script lang="ts">
  // Port of upstream livewire/project/database/backup-executions.blade.php
  // (Project\Database\BackupExecutions). S3 downloads through the instance download storage
  // (`PreparesBackupDownloads`) are not ported: the icon opens `/download/backup/{id}`.
  import { untrack } from 'svelte';
  import ConfirmModal from './ConfirmModal.svelte';
  import CopyInput from './CopyInput.svelte';
  import Empty from './Empty.svelte';
  import Reicon from './Reicon.svelte';
  import StatusBadge from './StatusBadge.svelte';
  import { ApiError } from '../lib/api';
  import { backupsUi, type BackupExecution } from '../lib/databaseApi';
  import { diffForHumans, duration, parseTimestamp } from '../lib/format';
  import { notify } from '../lib/toast.svelte';

  let { databaseUuid, backupUuid }: { databaseUuid: string; backupUuid: string } = $props();

  const take = 10;
  let executions = $state<BackupExecution[]>([]);
  let total = $state(0);
  let skip = $state(0);
  let timezone = $state<string | null>(null);
  let loaded = $state(false);
  let deleting = $state<BackupExecution | null>(null);
  let cleaning = $state(false);

  async function load() {
    const r = await backupsUi.executions(databaseUuid, backupUuid, skip);
    executions = r.executions;
    total = r.total;
    timezone = r.server_timezone;
    loaded = true;
  }
  $effect(() => {
    untrack(() => load());
    // `wire:poll.5000ms` on the first page.
    const poll = setInterval(() => skip === 0 && load().catch(() => {}), 5000);
    return () => clearInterval(poll);
  });

  /** `formatDateInServerTimezone`: `Y-m-d H:i:s T`. */
  function serverDate(ts: string | null | undefined): string {
    const d = parseTimestamp(ts);
    if (!d) return '';
    const zone = timezone || 'UTC';
    try {
      const parts = new Intl.DateTimeFormat('en-CA', {
        timeZone: zone,
        hour12: false,
        year: 'numeric',
        month: '2-digit',
        day: '2-digit',
        hour: '2-digit',
        minute: '2-digit',
        second: '2-digit',
        timeZoneName: 'short',
      }).formatToParts(d);
      const part = (t: string) => parts.find((p) => p.type === t)?.value ?? '';
      const hour = part('hour') === '24' ? '00' : part('hour');
      return `${part('year')}-${part('month')}-${part('day')} ${hour}:${part('minute')}:${part('second')} ${part('timeZoneName')}`;
    } catch {
      return d.toISOString();
    }
  }

  const headline = (s: string) => s.replace(/[_-]+/g, ' ').replace(/\b\w/g, (c) => c.toUpperCase());
  const statusOf = (e: BackupExecution): [string, 'neutral' | 'success' | 'warning' | 'error'] => {
    switch (e.status) {
      case 'success':
        return e.s3_uploaded === false ? ['S3 warning', 'warning'] : ['Success', 'success'];
      case 'running':
        return ['In progress', 'warning'];
      case 'failed':
        return ['Failed', 'error'];
      default:
        return [headline(e.status ?? ''), 'neutral'];
    }
  };

  /** `formatBytes`. */
  function formatBytes(value: string | number | null): string {
    const bytes = Number(value);
    if (!bytes) return '-';
    const units = ['B', 'KB', 'MB', 'GB', 'TB', 'PB'];
    const exp = Math.min(Math.floor(Math.log(bytes) / Math.log(1024)), units.length - 1);
    return `${Math.round((bytes / 1024 ** exp) * 100) / 100} ${units[exp]}`;
  }

  const finishedAt = (e: BackupExecution) => e.finished_at ?? e.updated_at;
  const hasLiveS3 = (e: BackupExecution) =>
    e.replicas.length ? e.replicas.some((r) => r.s3_uploaded === true && !r.s3_storage_deleted) : e.s3_uploaded === true && !e.s3_storage_deleted;
  const replicaStatus = (e: BackupExecution, r: BackupExecution['replicas'][number]) =>
    r.s3_storage_deleted ? 'deleted' : r.s3_uploaded === true ? null : r.s3_uploaded === false ? 'failed' : e.status === 'running' ? 'pending' : 'not uploaded';

  async function cleanupFailed() {
    await backupsUi.cleanupFailed(databaseUuid, backupUuid);
    await load();
  }

  async function cleanupDeleted(password: string) {
    try {
      await backupsUi.cleanupDeleted(databaseUuid, backupUuid, password);
      await load();
    } catch (e) {
      if (e instanceof ApiError && e.message === 'The provided password is incorrect.') return e.message;
      notify.failure(e instanceof ApiError ? e.message : String(e));
    }
  }

  async function deleteExecution(id: number, password: string, selected: string[]) {
    try {
      await backupsUi.deleteExecution(databaseUuid, backupUuid, id, password, selected);
      await load();
    } catch (e) {
      if (e instanceof ApiError && e.message === 'The provided password is incorrect.') return e.message;
      notify.failure(e instanceof ApiError ? e.message : String(e));
    }
  }
</script>

{#snippet avail(label: string, type: 'neutral' | 'success' | 'error', title?: string | null)}
  <span {title}><StatusBadge {label} {type} /></span>
{/snippet}

<div>
  <section class="application-settings-section overflow-hidden">
    <div class="application-settings-section-header">
      <div>
        <h2>Executions</h2>
      </div>
      <div class="flex flex-wrap items-center gap-2">
        <button type="button" class="button" onclick={cleanupFailed}>Clean failed backups</button>
        <button type="button" class="button button-error" onclick={() => (cleaning = true)}>Clean deleted entries</button>
      </div>
    </div>

    <div class="application-settings-section-body p-0!">
      {#if loaded && total === 0}
        <div class="p-4">
          <Empty size="sm" title="No backup executions" description="Execution history appears here after the schedule runs." iconName="browser-terminal" />
        </div>
      {:else if loaded}
        <div class="data-table deployment-table-scroll backup-executions-table-scroll">
          <div class="data-table-header backup-executions-table-grid h-auto rounded-none px-4 py-2.5 text-[11px]">
            <span>Status</span>
            <span>Database</span>
            <span>Archive</span>
            <span>Time</span>
            <span>Size</span>
            <span>Availability</span>
            <span class="text-right">Actions</span>
          </div>
          {#each executions as e (e.id)}
            {@const st = statusOf(e)}
            <div class="backup-execution-row">
              <div class="data-table-row backup-executions-table-grid min-h-14 border-b border-neutral-200 px-4 py-2.5 dark:border-white/[0.06]">
                <div class="flex items-center gap-2"><StatusBadge label={st[0]} type={st[1]} /></div>
                <div class="truncate text-[12px] font-medium text-black dark:text-fg">{e.database_name ?? 'N/A'}</div>
                <div class="min-w-0"><CopyInput text={e.filename ?? 'No archive name'} /></div>
                <div
                  class="text-[11px] text-neutral-500 dark:text-fg-faint"
                  title="Started: {serverDate(e.created_at)}{e.status !== 'running' ? `\nEnded: ${serverDate(finishedAt(e))}` : ''}"
                >
                  {#if e.status === 'running'}
                    Running for {duration(e.created_at, 'now')}
                  {:else}
                    <div class="text-black dark:text-fg">{serverDate(finishedAt(e))}</div>
                    <div>{diffForHumans(finishedAt(e))} · {duration(e.created_at, finishedAt(e))}</div>
                  {/if}
                </div>
                <div class="tabular-nums text-neutral-500 dark:text-fg-dim">{formatBytes(e.size)}</div>
                <div class="flex flex-wrap items-center gap-1.5">
                  <StatusBadge label={e.local_storage_deleted ? 'Local deleted' : 'Local'} type={e.local_storage_deleted ? 'neutral' : 'success'} />
                  {#each e.replicas as r (r.id)}
                    {@const status = replicaStatus(e, r)}
                    {@render avail(
                      `${r.s3_name ?? 'Removed S3 storage'}${status ? ` ${status}` : ''}`,
                      r.s3_storage_deleted || r.s3_uploaded === null ? 'neutral' : r.s3_uploaded ? 'success' : 'error',
                      r.message,
                    )}
                  {/each}
                  {#if !e.replicas.length && e.s3_uploaded !== null}
                    <StatusBadge
                      label={e.s3_storage_deleted ? 'S3 deleted' : e.s3_uploaded ? 'S3' : 'S3 failed'}
                      type={e.s3_storage_deleted ? 'neutral' : e.s3_uploaded ? 'success' : 'error'}
                    />
                  {/if}
                </div>
                <div class="flex items-center justify-end gap-1">
                  {#if e.status === 'success'}
                    <button
                      type="button"
                      class="icon-button shrink-0"
                      onclick={() => window.open(`/download/backup/${e.id}`, '_blank')}
                      title="Download backup"
                      aria-label="Download backup"
                    >
                      <Reicon name="upload" class="size-3.5 rotate-180" />
                    </button>
                  {/if}
                  <button
                    type="button"
                    class="icon-button shrink-0 text-red-500 hover:text-red-600 dark:text-red-400 dark:hover:text-red-300"
                    title="Delete backup"
                    aria-label="Delete backup"
                    onclick={() => (deleting = e)}
                  >
                    <Reicon name="trash" class="size-3.5" />
                  </button>
                </div>
              </div>
              {#if e.message}
                <div class="border-t border-neutral-200 bg-neutral-50 px-4 py-3 dark:border-white/[0.06] dark:bg-white/[0.02]">
                  <pre class="max-h-48 overflow-auto whitespace-pre-wrap rounded-lg bg-neutral-100 p-3 font-mono text-xs leading-5 text-neutral-700 dark:bg-black/20 dark:text-fg-dim">{e.message}</pre>
                </div>
              {/if}
            </div>
          {/each}
        </div>

        <div class="flex min-h-11 items-center justify-between border-t border-neutral-200 px-4 text-[11px] text-neutral-500 dark:border-white/[0.08] dark:text-fg-faint">
          <span>{skip + 1}-{Math.min(skip + take, total)} of {total}</span>
          <div class="flex items-center gap-1">
            <button
              type="button"
              class="icon-button"
              disabled={skip === 0}
              aria-label="Previous page"
              onclick={() => {
                skip = Math.max(0, skip - take);
                load();
              }}><Reicon name="arrow-right" class="size-3.5 rotate-180" /></button
            >
            <button
              type="button"
              class="icon-button"
              disabled={executions.length < take}
              aria-label="Next page"
              onclick={() => {
                skip += take;
                load();
              }}><Reicon name="arrow-right" class="size-3.5" /></button
            >
          </div>
        </div>
      {/if}
    </div>
  </section>
</div>

{#if cleaning}
  <ConfirmModal
    title="Cleanup Deleted Backup Entries?"
    actions={['Permanently delete execution records already removed from local storage.', 'Actual backup files are not changed.']}
    confirmationText="cleanup deleted backups"
    confirmationLabel="Type cleanup deleted backups to confirm."
    shortConfirmationLabel="Confirmation"
    confirmWithPassword
    onconfirm={(password) => cleanupDeleted(password)}
    onclose={() => (cleaning = false)}
  />
{/if}

{#if deleting}
  {@const e = deleting}
  <ConfirmModal
    title="Confirm Backup Deletion?"
    checkboxes={hasLiveS3(e) ? [{ id: 'delete_backup_s3', label: 'Delete the selected backup permanently from every S3 storage' }] : []}
    actions={e.local_storage_deleted ? ['This backup execution record will be deleted.'] : ['This backup will be permanently deleted from local storage.']}
    confirmationText={e.filename ?? ''}
    confirmationLabel="Enter the backup filename to confirm."
    shortConfirmationLabel="Backup Filename"
    confirmWithPassword
    onconfirm={(password, selected) => deleteExecution(e.id, password, selected)}
    onclose={() => (deleting = null)}
  />
{/if}
