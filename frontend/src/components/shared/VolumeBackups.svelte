<script lang="ts">
  // Port of upstream livewire/project/shared/storages/volume-backups.blade.php and its partials
  // general, s3, retention, danger and executions (Project/Shared/Storages/VolumeBackups).
  import { untrack } from 'svelte';
  import Callout from '../Callout.svelte';
  import ConfirmModal from '../ConfirmModal.svelte';
  import CopyInput from '../CopyInput.svelte';
  import DangerZone from '../DangerZone.svelte';
  import Empty from '../Empty.svelte';
  import FormInput from '../FormInput.svelte';
  import Listbox from '../Listbox.svelte';
  import Pager from '../Pager.svelte';
  import Reicon from '../Reicon.svelte';
  import RowActions from '../RowActions.svelte';
  import RowAction from '../RowAction.svelte';
  import SettingsSection from '../SettingsSection.svelte';
  import StatusBadge from '../StatusBadge.svelte';
  import UnsavedBar from '../UnsavedBar.svelte';
  import { teleport } from '../../lib/databaseMenu';
  import { ApiError } from '../../lib/api';
  import { diffForHumans, duration, parseTimestamp } from '../../lib/format';
  import { link, router } from '../../lib/router.svelte';
  import { notify } from '../../lib/toast.svelte';
  import {
    downloadBackup,
    formatBytes,
    volumeBackupsUi,
    type BackupOwner,
    type VolumeBackupForm,
    type VolumeBackupState,
    type VolumeExecution,
  } from '../../lib/volumeBackupsApi';

  let {
    kind,
    uuid,
    backupUuid,
    section,
    onexecutions,
    ondeleted,
    actionsInHeader = false,
    canUpdate = true,
  }: {
    kind: BackupOwner;
    uuid: string;
    backupUuid: string;
    section: 'general' | 'executions' | 'danger';
    /** `redirect()->route($this->routeName('executions'), ...)`. */
    onexecutions?: () => void;
    ondeleted?: () => void;
    /** `$actionsInHeader`: the page shows the backup actions in the top header bar. */
    actionsInHeader?: boolean;
    canUpdate?: boolean;
  } = $props();

  let state_ = $state<VolumeBackupState | null>(null);
  let form = $state<VolumeBackupForm | null>(null);
  let saved = $state('');
  let saving = $state(false);
  let deleting = $state(false);
  let error = $state<ApiError | null>(null);

  // The inputs hold text, whatever the columns store (`0`, `3600.0`, null).
  const clone = (f: VolumeBackupForm): VolumeBackupForm => {
    const out: Record<string, unknown> = { ...f };
    for (const [k, v] of Object.entries(f)) {
      if (typeof v === 'number') out[k] = String(v);
      else if (v === null) out[k] = '';
    }
    return out as unknown as VolumeBackupForm;
  };
  function apply(s: VolumeBackupState) {
    state_ = s;
    form = clone(s.form);
    saved = JSON.stringify(form);
  }
  const dirty = $derived(form !== null && JSON.stringify(form) !== saved);

  async function load() {
    apply(await volumeBackupsUi.show(kind, uuid, backupUuid));
  }

  async function run(action: 'save' | 's3Storages' | 'toggleS3' | 'toggleEnabled') {
    saving = true;
    error = null;
    try {
      const r = await volumeBackupsUi.action(kind, uuid, backupUuid, action, form ? $state.snapshot(form) : undefined);
      // A failed save leaves what the user typed (Livewire keeps its properties).
      if (r.backup && r.ok) apply(r.backup);
    } catch (e) {
      if (e instanceof ApiError && e.status === 422) error = e;
      else notify.failure(e instanceof ApiError ? e.message : String(e));
    } finally {
      saving = false;
    }
  }

  async function backupNow() {
    const r = await volumeBackupsUi.action(kind, uuid, backupUuid, 'backupNow');
    if (r.redirect_executions) onexecutions?.();
  }

  async function removeSchedule(password: string, selected: string[]) {
    try {
      const r = await volumeBackupsUi.remove(kind, uuid, backupUuid, password, selected);
      if (r.deleted) ondeleted?.();
    } catch (e) {
      if (e instanceof ApiError && e.message === 'The provided password is incorrect.') return e.message;
      throw e;
    }
  }

  // Executions
  let executions = $state<VolumeExecution[]>([]);
  let total = $state(0);
  let page = $state(1);
  let perPage = $state(10);
  let timezone = $state<string | null>(null);
  let s3Downloads = $state(false);
  let loaded = $state(false);
  let deletingExecution = $state<VolumeExecution | null>(null);
  let cleaning = $state(false);

  async function retryRecovery(id: number) {
    await volumeBackupsUi.retryRecovery(kind, uuid, backupUuid, id);
    await loadExecutions();
  }
  async function loadExecutions() {
    const r = await volumeBackupsUi.executions(kind, uuid, backupUuid, page, perPage);
    executions = r.executions;
    total = r.total;
    timezone = r.server_timezone;
    s3Downloads = r.s3_downloads;
    loaded = true;
  }

  $effect(() => {
    untrack(() => load());
  });
  $effect(() => {
    void page;
    void perPage;
    if (section !== 'executions') return;
    untrack(() => loadExecutions());
    // `wire:poll.5000ms`.
    const poll = setInterval(() => loadExecutions().catch(() => {}), 5000);
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
  const statusOf = (e: VolumeExecution): [string, 'neutral' | 'success' | 'warning' | 'error'] => {
    if (e.status === 'running') return ['In progress', 'warning'];
    if (e.status === 'success') return e.s3_uploaded === false ? ['S3 warning', 'warning'] : [headline(e.status), 'success'];
    if (e.status === 'failed') return [headline(e.status), 'error'];
    return [headline(e.status ?? ''), 'neutral'];
  };
  const hasLiveS3 = (e: VolumeExecution) =>
    e.replicas.length ? e.replicas.some((r) => r.s3_uploaded === true && !r.s3_storage_deleted) : e.s3_uploaded === true && !e.s3_storage_deleted;
  const isDownloadable = (e: VolumeExecution) => !e.local_storage_deleted || e.replicas.some((r) => r.s3_uploaded === true && !r.s3_storage_deleted);
  const recoveryPending = (e: VolumeExecution) => e.status !== 'running' && (e.stop_recovery_pending || e.s3_cleanup_pending);
  const recoveryLabel = (e: VolumeExecution) =>
    ({ s3_auth: 'S3 credentials', s3_bucket: 'S3 bucket missing', server_unreachable: 'server unreachable', remote_command: 'remote command failed' })[e.recovery_error ?? ''] ??
    'unknown error';
  const replicaStatus = (e: VolumeExecution, r: VolumeExecution['replicas'][number]) =>
    r.s3_storage_deleted ? ' deleted' : r.s3_uploaded === true ? '' : r.s3_uploaded === false ? ' failed' : e.status === 'running' ? ' pending' : ' not uploaded';
  const finishedAt = (e: VolumeExecution) => e.finished_at ?? e.updated_at;

  async function cleanupDeleted(password: string) {
    try {
      await volumeBackupsUi.cleanupDeleted(kind, uuid, backupUuid, password);
      await loadExecutions();
    } catch (e) {
      if (e instanceof ApiError && e.message === 'The provided password is incorrect.') return e.message;
      notify.failure(e instanceof ApiError ? e.message : String(e));
    }
  }
  async function deleteExecution(id: number, password: string, selected: string[]) {
    try {
      await volumeBackupsUi.deleteExecution(kind, uuid, backupUuid, id, password, selected);
      await loadExecutions();
    } catch (e) {
      if (e instanceof ApiError && e.message === 'The provided password is incorrect.') return e.message;
      notify.failure(e instanceof ApiError ? e.message : String(e));
    }
  }
</script>

{#snippet headerActions()}
  {#if form}
    {#if !form.enabled}
      <button type="button" class="button button-highlighted" onclick={() => run('toggleEnabled')}>Enable backup</button>
    {:else}
      <button type="button" class="button" onclick={() => run('toggleEnabled')}>Disable backup</button>
    {/if}
    <button type="button" class="button" onclick={backupNow}>Back up now</button>
  {/if}
{/snippet}

{#if state_ && form}
  <div>
    {#if actionsInHeader && canUpdate}
      <div class="order-first mr-1 hidden items-center gap-1 xl:flex" use:teleport={'resource-action-hud-slot'}>
        {@render headerActions()}
      </div>
    {/if}

    {#if section === 'executions'}
      <div>
        <SettingsSection title="Executions" description="Review generated archives, availability, and cleanup status." flush>
          {#snippet actions()}
            <button
              type="button"
              class="button"
              onclick={async () => {
                await volumeBackupsUi.cleanupFailed(kind, uuid, backupUuid);
                await loadExecutions();
              }}>Cleanup failed</button
            >
            <button type="button" class="button button-error" onclick={() => (cleaning = true)}>Cleanup deleted</button>
          {/snippet}

          {#if loaded && total > 0}
            <div class="data-table w-full overflow-x-auto">
              <div class="data-table-header volume-backup-executions-grid border-b border-neutral-200 bg-neutral-50 dark:border-white/[0.08] dark:bg-white/[0.05]">
                <span>Status</span>
                <span>Archive</span>
                <span>Time</span>
                <span>Size</span>
                <span>Availability</span>
                <span class="text-right">Actions</span>
              </div>

              {#each executions as e (e.id)}
                {@const st = statusOf(e)}
                <div class="data-table-row volume-backup-executions-grid min-h-16 items-center gap-x-3 border-b border-neutral-200 text-[12px] last:border-b-0 dark:border-white/[0.07]">
                  <span><StatusBadge label={st[0]} type={st[1]} /></span>
                  <span class="min-w-0"><CopyInput text={e.filename ?? 'No archive name'} /></span>
                  <span class="text-[11px] text-neutral-500 dark:text-fg-faint" title="Started: {serverDate(e.created_at)}{e.status !== 'running' ? `\nEnded: ${serverDate(finishedAt(e))}` : ''}">
                    {#if e.status === 'running'}
                      Running for {duration(e.created_at, 'now')}
                    {:else}
                      <span class="block text-black dark:text-fg">{serverDate(finishedAt(e))}</span>
                      {diffForHumans(finishedAt(e))} · {duration(e.created_at, finishedAt(e))}
                    {/if}
                  </span>
                  <span class="tabular-nums text-neutral-500 dark:text-fg-dim">{Number(e.size) > 0 ? formatBytes(e.size) : '-'}</span>
                  <span class="flex flex-wrap gap-1">
                    <StatusBadge label={e.local_storage_deleted ? 'Local deleted' : 'Local'} type={e.local_storage_deleted ? 'neutral' : 'success'} />
                    {#each e.replicas as r (r.id)}
                      <span title={r.message}>
                        <StatusBadge
                          label="{r.s3_name ?? 'Removed S3 storage'}{replicaStatus(e, r)}"
                          type={r.s3_storage_deleted || r.s3_uploaded === null ? 'neutral' : r.s3_uploaded ? 'success' : 'error'}
                        />
                      </span>
                    {/each}
                    {#if !e.replicas.length && e.s3_uploaded !== null}
                      <StatusBadge label={e.s3_storage_deleted ? 'S3 deleted' : e.s3_uploaded ? 'S3' : 'S3 failed'} type={e.s3_storage_deleted ? 'neutral' : e.s3_uploaded ? 'success' : 'error'} />
                    {/if}
                    {#if recoveryPending(e)}
                      {#if e.recovery_needs_attention}
                        <StatusBadge label="Needs attention ({recoveryLabel(e)})" type="error" />
                      {:else}
                        <span title={e.recovery_error ? `Last attempt: ${recoveryLabel(e)}` : undefined}><StatusBadge label="Recovery pending" type="warning" /></span>
                      {/if}
                    {/if}
                  </span>
                  <span class="flex items-center justify-end gap-1">
                    {#if e.status === 'success' && isDownloadable(e)}
                      <button type="button" class="icon-button shrink-0" onclick={() => downloadBackup('storage', e.id, s3Downloads)} title="Download backup" aria-label="Download backup">
                        <Reicon name="upload" class="size-3.5 rotate-180" />
                      </button>
                    {/if}
                    {#if recoveryPending(e) && e.status !== 'running'}
                      <RowActions label="Backup actions">
                        <RowAction onclick={() => retryRecovery(e.id)}>Retry recovery</RowAction>
                        {#snippet danger()}
                          <RowAction onclick={() => (deletingExecution = e)}>Delete</RowAction>
                        {/snippet}
                      </RowActions>
                    {:else if recoveryPending(e)}
                      <button type="button" class="icon-button shrink-0" onclick={() => retryRecovery(e.id)} title="Retry recovery" aria-label="Retry recovery">
                        <Reicon name="refresh" class="size-3.5" />
                      </button>
                    {:else if e.status !== 'running'}
                      <button
                        type="button"
                        class="icon-button shrink-0 text-red-500 hover:text-red-600 dark:text-red-400 dark:hover:text-red-300"
                        title="Delete backup"
                        aria-label="Delete backup"
                        onclick={() => (deletingExecution = e)}
                      >
                        <Reicon name="trash" class="size-3.5" />
                      </button>
                    {/if}
                  </span>
                  {#if e.message}
                    <pre class="volume-backup-execution-message col-span-6 min-w-0 max-w-full max-h-20 overflow-y-auto overflow-x-hidden bg-transparent py-2 text-[11px] break-words whitespace-pre-wrap text-neutral-600 dark:text-fg-dim">{e.message}</pre>
                  {/if}
                </div>
              {/each}
              {#if total > 10}
                <Pager {total} bind:page bind:pageSize={perPage} separator="–" storageKey="coolify.page-size.volume-backup-executions" />
              {/if}
            </div>
          {:else if loaded}
            <Empty size="sm" title="No executions" description="Run the backup schedule to create its first execution." iconName="storages" />
          {/if}
        </SettingsSection>
      </div>
    {:else if section === 'danger'}
      <div class="application-settings-form">
        <SettingsSection class="danger-section" title="Delete backup schedule" description="Permanently delete this schedule and optionally delete its local and S3 archives.">
          <DangerZone title="This action cannot be undone.">
            <p>You can select which backup archives to remove.</p>
            {#snippet action()}
              <button type="button" class="button button-error" onclick={() => (deleting = true)}>Delete schedule</button>
            {/snippet}
          </DangerZone>
        </SettingsSection>
      </div>
    {:else}
      <form
        class="application-settings-form flex flex-col gap-6"
        onsubmit={(e) => {
          e.preventDefault();
          run('save');
        }}
      >
        <UnsavedBar {dirty} {saving} onsave={() => run('save')} onreset={() => (form = JSON.parse(saved))} />

        <SettingsSection title="Backup schedule" description="Choose when this storage is archived and how long each backup may run.">
          {#snippet actions()}
            <!-- Below xl the header bar is hidden, so keep the buttons here. -->
            <div class="flex items-center gap-2 {actionsInHeader ? 'xl:hidden' : ''}">
              {@render headerActions()}
            </div>
          {/snippet}

          <div class="mb-4 flex items-center justify-between gap-3 rounded-lg bg-neutral-50 px-3 py-2.5 ring-1 ring-neutral-200 dark:bg-white/[0.05] dark:ring-white/[0.07]">
            <span class="text-[12px] text-neutral-500 dark:text-fg-dim">{state_.target_type}</span>
            <span class="truncate text-[12px] font-medium text-neutral-900 dark:text-fg">{state_.target_name}</span>
          </div>

          <Callout type="warning" title="File-level consistency">
            Archives created while the application writes to this storage can be inconsistent. Stopping containers during the archive is safer, but briefly interrupts the application.
            <div class="mt-4">
              <Listbox
                id="stopDuringBackup"
                label="Archive behavior"
                bind:value={form.stop_during_backup}
                onchange={() => run('save')}
                options={[
                  { value: false, label: 'Keep containers running' },
                  { value: true, label: 'Stop containers during archive' },
                ]}
              />
            </div>
          </Callout>

          <div class="mt-4 grid gap-4 lg:grid-cols-2">
            <FormInput id="frequency" label="Frequency" required helper="Use every_minute, hourly, daily, weekly, monthly, yearly, or a cron expression." bind:value={form.frequency} error={error?.field('frequency')} />
            <FormInput id="timezone" label="Timezone" disabled helper="Uses the backup server timezone, or the instance timezone when none is configured." required value={state_.timezone} />
            <FormInput id="timeout" type="number" min="60" max="36000" label="Timeout" helper="Maximum backup runtime in seconds." required bind:value={form.timeout} error={error?.field('timeout')} />
            <FormInput
              label="Missing backup alert after"
              id="missingBackupNotificationDays"
              type="number"
              min="0"
              max="365"
              helper="Notify through backup failure channels after this many days without an execution. Use 0 to disable."
              required
              bind:value={form.missing_backup_notification_days}
              error={error?.field('missing_backup_notification_days')}
            />
          </div>
        </SettingsSection>

        {#if state_.storages.length === 0}
          <SettingsSection title="S3 storage" description="Send backup archives to a validated object storage destination." flush>
            <Empty title="No validated S3 storage" description="Add and validate an S3 storage destination before enabling remote backups." iconName="storages">
              {#snippet contents()}
                <a class="button" use:link href="/storages">Open S3 storage</a>
              {/snippet}
            </Empty>
          </SettingsSection>
        {:else}
          <SettingsSection title="S3 storage" description="Choose where remote backups are stored and whether local copies are retained.">
            {#snippet actions()}
              {#if !form?.save_s3}
                <button type="button" class="button button-highlighted" onclick={() => run('toggleS3')}>Enable S3</button>
              {:else}
                <button type="button" class="button" onclick={() => run('toggleS3')}>Disable S3</button>
              {/if}
            {/snippet}
            <div class="grid gap-4 sm:grid-cols-2">
              <Listbox
                id="s3StorageIds"
                label="S3 storages"
                multiple
                helper="Each backup is uploaded to every selected storage."
                placeholder="Select S3 storages"
                error={error?.field('s3StorageIds')}
                bind:values={
                  () => (form?.s3_storage_ids ?? []).map(String),
                  (v) => {
                    if (!form) return;
                    form.s3_storage_ids = v.map(Number);
                    run('s3Storages');
                  }
                }
                options={state_.storages.map((s) => ({ value: String(s.id), label: s.name + (s.is_usable ? '' : ' (unavailable)') }))}
              />
              <Listbox
                id="disableLocalBackup"
                label="Local copy"
                disabled={!form.save_s3}
                bind:value={form.disable_local_backup}
                onchange={() => run('save')}
                options={[
                  { value: false, label: 'Keep local backup' },
                  { value: true, label: 'Delete after S3 upload' },
                ]}
              />
            </div>
          </SettingsSection>
        {/if}

        <SettingsSection title="Retention" description="The first reached limit removes the oldest archive. Use 0 for unlimited retention.">
          <div class="space-y-6">
            <div>
              <h3 class="mb-3 text-sm font-semibold text-black dark:text-fg">Local archives</h3>
              <div class="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
                <FormInput label="Archives to keep" id="retentionAmountLocally" type="number" min="0" max="10000" helper="Maximum number of recent local archives." required bind:value={form.retention_amount_locally} error={error?.field('retention_amount_locally')} />
                <FormInput label="Days to keep" id="retentionDaysLocally" type="number" min="0" helper="Remove local archives older than this many days." required bind:value={form.retention_days_locally} error={error?.field('retention_days_locally')} />
                <FormInput label="Maximum storage (GB)" id="retentionMaxStorageLocally" type="number" min="0" step="any" helper="Remove oldest local archives after this total size is reached." required bind:value={form.retention_max_storage_locally} error={error?.field('retention_max_storage_locally')} />
              </div>
            </div>
            <div class="border-t border-neutral-200 pt-6 dark:border-white/[0.06]">
              <h3 class="mb-3 text-sm font-semibold text-black dark:text-fg">S3 archives</h3>
              <div class="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
                <FormInput label="Archives to keep" id="retentionAmountS3" type="number" min="0" max="10000" helper="Maximum number of recent S3 archives." required bind:value={form.retention_amount_s3} error={error?.field('retention_amount_s3')} />
                <FormInput label="Days to keep" id="retentionDaysS3" type="number" min="0" helper="Remove S3 archives older than this many days." required bind:value={form.retention_days_s3} error={error?.field('retention_days_s3')} />
                <FormInput label="Maximum storage (GB)" id="retentionMaxStorageS3" type="number" min="0" step="any" helper="Remove oldest S3 archives after this total size is reached." required bind:value={form.retention_max_storage_s3} error={error?.field('retention_max_storage_s3')} />
              </div>
            </div>
          </div>
        </SettingsSection>
      </form>
    {/if}
  </div>
{/if}

{#if deleting && state_}
  <ConfirmModal
    title="Delete backup schedule?"
    checkboxes={[
      { id: 'delete_associated_backups_locally', label: 'Delete all local archives created by this schedule.' },
      { id: 'delete_associated_backups_s3', label: 'Delete all S3 archives created by this schedule.' },
    ]}
    actions={['Delete the selected backup schedule.']}
    confirmationText={state_.target_name}
    confirmationLabel="Enter the {state_.target_type} identifier to confirm."
    shortConfirmationLabel="{state_.target_type} identifier"
    confirmWithPassword
    onconfirm={removeSchedule}
    onclose={() => (deleting = false)}
  />
{/if}

{#if cleaning}
  <ConfirmModal
    title="Cleanup Deleted Backup Entries?"
    actions={['This permanently deletes backup execution entries whose local and S3 files have already been deleted.', 'This only removes database entries, not backup files.']}
    confirmationText="cleanup deleted backups"
    confirmationLabel="Please confirm by typing 'cleanup deleted backups' below"
    shortConfirmationLabel="Confirmation"
    confirmWithPassword
    onconfirm={(password) => cleanupDeleted(password)}
    onclose={() => (cleaning = false)}
  />
{/if}

{#if deletingExecution}
  {@const e = deletingExecution}
  <ConfirmModal
    title="Confirm Backup Deletion?"
    checkboxes={hasLiveS3(e) ? [{ id: 'delete_backup_s3', label: 'Delete the selected backup permanently from every S3 storage' }] : []}
    actions={e.local_storage_deleted ? ['This backup execution record will be deleted.'] : ['This backup will be permanently deleted from local storage.']}
    confirmationText={e.filename ?? ''}
    confirmationLabel="Please confirm the execution of the actions by entering the Backup Filename below"
    shortConfirmationLabel="Backup Filename"
    confirmWithPassword
    onconfirm={(password, selected) => deleteExecution(e.id, password, selected)}
    onclose={() => (deletingExecution = null)}
  />
{/if}
