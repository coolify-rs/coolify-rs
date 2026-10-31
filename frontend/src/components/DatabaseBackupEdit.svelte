<script lang="ts">
  // Port of upstream livewire/project/database/backup-edit.blade.php (Project\Database\BackupEdit):
  // backup-edit/general, s3, retention and danger.
  import { untrack } from 'svelte';
  import ConfirmModal from './ConfirmModal.svelte';
  import DangerZone from './DangerZone.svelte';
  import Empty from './Empty.svelte';
  import FormInput from './FormInput.svelte';
  import Listbox from './Listbox.svelte';
  import Reicon from './Reicon.svelte';
  import SettingsSection from './SettingsSection.svelte';
  import UnsavedBar from './UnsavedBar.svelte';
  import { ApiError, type DatabaseRecord } from '../lib/api';
  import { backupsUi, type BackupEdit, type BackupForm } from '../lib/databaseApi';
  import { link, router } from '../lib/router.svelte';
  import { notify } from '../lib/toast.svelte';

  let {
    record,
    backupUuid,
    section,
    base,
    edit,
    canUpdate,
    onchanged,
    actionsInHeader = true,
  }: {
    record: DatabaseRecord;
    backupUuid: string;
    section: string;
    base: string;
    edit: BackupEdit;
    canUpdate: boolean;
    onchanged: (e: BackupEdit) => void;
    /** The page shows the actions in the top header bar above xl; the schedule modal of a service does not. */
    actionsInHeader?: boolean;
  } = $props();

  // The inputs hold text, whatever the columns store (`0`, `3600.0`, null).
  const clone = (f: BackupForm): BackupForm => {
    const out: Record<string, unknown> = { ...f };
    for (const [k, v] of Object.entries(f)) {
      if (typeof v === 'number') out[k] = String(v);
      else if (v === null) out[k] = '';
    }
    return out as unknown as BackupForm;
  };
  let form = $state<BackupForm>(untrack(() => clone(edit.form)));
  let saved = $state(untrack(() => JSON.stringify(clone(edit.form))));
  let saving = $state(false);
  let deleting = $state(false);
  let draft = $state('');
  const dirty = $derived(JSON.stringify(form) !== saved);
  const running = $derived((record.status ?? '').startsWith('running'));
  const type = $derived(edit.database_type);
  const notRunningTip = 'The database must be running to start a backup.';

  const databases = $derived((form.databases_to_backup ?? '').split(',').map((n) => n.trim()).filter(Boolean));
  function addDatabase() {
    const names = draft.split(',').map((n) => n.trim()).filter(Boolean);
    if (!names.length) return;
    form.databases_to_backup = [...new Set([...databases, ...names])].join(',');
    draft = '';
  }
  const removeDatabase = (index: number) => (form.databases_to_backup = databases.filter((_, i) => i !== index).join(','));

  function apply(e: BackupEdit) {
    form = clone(e.form);
    saved = JSON.stringify(form);
    onchanged(e);
  }

  async function run(action: 'submit' | 'toggleS3') {
    saving = true;
    try {
      const r = await backupsUi.action(record.uuid, backupUuid, action, $state.snapshot(form));
      // A failed save leaves what the user typed (Livewire keeps its properties).
      if (r.backup && r.ok) apply(r.backup);
    } catch (e) {
      notify.failure(e instanceof ApiError ? e.message : String(e));
    } finally {
      saving = false;
    }
  }

  async function toggleEnabled() {
    const r = await backupsUi.action(record.uuid, backupUuid, 'toggleEnabled');
    if (r.backup) {
      form.enabled = r.backup.form.enabled;
      const s = JSON.parse(saved);
      s.enabled = r.backup.form.enabled;
      saved = JSON.stringify(s);
      onchanged(r.backup);
    }
  }

  async function backupNow() {
    const r = await backupsUi.action(record.uuid, backupUuid, 'backupNow');
    if (r.redirect_executions) router.go(`${base}/backups/${backupUuid}/executions`);
  }

  async function remove(password: string, selected: string[]) {
    try {
      const r = await backupsUi.remove(record.uuid, backupUuid, password, selected);
      if (r.deleted) router.go(`${base}/backups`);
    } catch (e) {
      if (e instanceof ApiError && e.message === 'The provided password is incorrect.') return e.message;
      throw e;
    }
  }
</script>

<div class="flex flex-col gap-6">
  {#if section === 'danger'}
    <div class="application-settings-form">
      <SettingsSection title="Delete backup schedule" description="Permanently remove this schedule and optionally its backup archives.">
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
      class="flex flex-col gap-6"
      onsubmit={(e) => {
        e.preventDefault();
        run('submit');
      }}
    >
      <UnsavedBar {dirty} {saving} onsave={() => run('submit')} onreset={() => (form = JSON.parse(saved))} />

      <SettingsSection title="Backup schedule" description="Choose what to back up, when it runs, and how long it may run.">
        {#snippet actions()}
          <div class="flex items-center gap-2 {actionsInHeader ? 'xl:hidden' : ''}">
            {#if !form.enabled}
              <button type="button" class="button button-highlighted" onclick={toggleEnabled}>Enable backup</button>
            {:else}
              <button type="button" class="button" onclick={toggleEnabled}>Disable backup</button>
            {/if}
            <button type="button" class="button" disabled={!running} title={running ? undefined : notRunningTip} onclick={backupNow}>Back up now</button>
          </div>
        {/snippet}

        <div class="space-y-5">
          {#if type === 'App\\Models\\StandalonePostgresql' || type === 'App\\Models\\StandaloneMysql' || type === 'App\\Models\\StandaloneMariadb'}
            <div class="grid w-full gap-4">
              <Listbox
                id="dumpAll"
                label="Database selection"
                bind:value={form.dump_all}
                onchange={() => run('submit')}
                options={[
                  { value: true, label: 'All databases' },
                  { value: false, label: 'Specific databases' },
                ]}
              />
              {#if !form.dump_all}
                <div class="w-full">
                  <!-- svelte-ignore a11y_label_has_associated_control -->
                  <label class="mb-1.5 block text-sm font-medium">Databases to back up</label>
                  <div class="chip-input">
                    {#each databases as database, index (database)}
                      <span class="chip font-mono">
                        <span>{database}</span>
                        <button type="button" onclick={() => removeDatabase(index)} class="chip-remove" aria-label="Remove {database}">
                          <Reicon name="x" class="size-3" />
                        </button>
                      </span>
                    {/each}
                    <input
                      bind:value={draft}
                      onkeydown={(e) => {
                        if (e.key === 'Enter' || e.key === ',') {
                          e.preventDefault();
                          addDatabase();
                        }
                      }}
                      onblur={addDatabase}
                      type="text"
                      placeholder="Type a database and press Enter"
                    />
                  </div>
                  <p class="mt-1.5 text-xs text-neutral-500 dark:text-fg-dim">Add one or more database names. Leave empty to include the default database.</p>
                </div>
              {/if}
            </div>
          {:else if type === 'App\\Models\\StandaloneMongodb'}
            <FormInput
              label="Databases to include"
              helper="Use database:collection1,collection2|database2 to exclude selected collections. Leave empty to include all databases and collections."
              id="databasesToBackup"
              bind:value={form.databases_to_backup}
            />
          {:else if type === 'App\\Models\\StandaloneClickhouse'}
            <FormInput label="Databases to back up" helper="Comma-separated database names. Leave empty to include the default database." id="databasesToBackup" bind:value={form.databases_to_backup} />
          {:else if type === 'App\\Models\\StandaloneSqlite'}
            <FormInput
              label="Database files to back up"
              helper="Comma-separated file names inside the data volume. Leave empty to back up every configured file."
              id="databasesToBackup"
              bind:value={form.databases_to_backup}
            />
          {/if}

          <div class="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
            <FormInput label="Frequency" id="frequency" required bind:value={form.frequency} />
            <FormInput label="Timezone" id="timezone" disabled helper="Uses the deployment server timezone, or the instance timezone when none is configured." required value={edit.timezone} />
            <FormInput label="Timeout" id="timeout" type="number" min="60" helper="Maximum backup runtime in seconds." required bind:value={form.timeout} />
            <FormInput
              label="Missing backup alert after"
              id="missingBackupNotificationDays"
              type="number"
              min="0"
              max="365"
              disabled={!canUpdate}
              helper="Notify through backup failure channels after this many days without an execution. Use 0 to disable."
              required
              bind:value={form.missing_backup_notification_days}
            />
          </div>
        </div>
      </SettingsSection>

      {#if edit.storages.length === 0}
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
            {#if !form.save_s3}
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
              bind:values={
                () => form.s3_storage_ids.map(String),
                (v) => {
                  form.s3_storage_ids = v.map(Number);
                  run('submit');
                }
              }
              options={edit.storages.map((s) => ({ value: String(s.id), label: s.name + (s.is_usable ? '' : ' (unavailable)') }))}
            />
            <Listbox
              id="disableLocalBackup"
              label="Local copy"
              disabled={!form.save_s3}
              bind:value={form.disable_local_backup}
              options={[
                { value: false, label: 'Keep local backup' },
                { value: true, label: 'Delete after S3 upload' },
              ]}
            />
          </div>
        </SettingsSection>
      {/if}

      <SettingsSection title="Retention" description="The first reached limit removes the oldest backup. Use 0 for unlimited retention.">
        <div class="space-y-6">
          <div>
            <h3 class="mb-3 text-sm font-semibold text-black dark:text-fg">Local backups</h3>
            <div class="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
              <FormInput label="Backups to keep" id="databaseBackupRetentionAmountLocally" type="number" min="0" helper="Maximum number of recent local backups." required bind:value={form.database_backup_retention_amount_locally} />
              <FormInput label="Days to keep" id="databaseBackupRetentionDaysLocally" type="number" min="0" helper="Remove local backups older than this many days." required bind:value={form.database_backup_retention_days_locally} />
              <FormInput label="Maximum storage (GB)" id="databaseBackupRetentionMaxStorageLocally" type="number" min="0" step="any" helper="Remove oldest local backups after this total size is reached." required bind:value={form.database_backup_retention_max_storage_locally} />
            </div>
          </div>
          <div class="border-t border-neutral-200 pt-6 dark:border-white/[0.06]">
            <h3 class="mb-3 text-sm font-semibold text-black dark:text-fg">S3 backups</h3>
            <div class="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
              <FormInput label="Backups to keep" id="databaseBackupRetentionAmountS3" type="number" min="0" helper="Maximum number of recent S3 backups." required bind:value={form.database_backup_retention_amount_s3} />
              <FormInput label="Days to keep" id="databaseBackupRetentionDaysS3" type="number" min="0" helper="Remove S3 backups older than this many days." required bind:value={form.database_backup_retention_days_s3} />
              <FormInput label="Maximum storage (GB)" id="databaseBackupRetentionMaxStorageS3" type="number" min="0" step="any" helper="Remove oldest S3 backups after this total size is reached." required bind:value={form.database_backup_retention_max_storage_s3} />
            </div>
          </div>
        </div>
      </SettingsSection>
    </form>
  {/if}
</div>

{#if deleting}
  <ConfirmModal
    title="Confirm Backup Schedule Deletion?"
    checkboxes={[
      { id: 'delete_associated_backups_locally', label: 'All backups will be permanently deleted from local storage.' },
      { id: 'delete_associated_backups_s3', label: 'All backups will be permanently deleted (associated with this backup job) from every S3 storage of this schedule.' },
    ]}
    actions={['The selected backup schedule will be deleted.', 'Scheduled backups for this database will stop if this is its only schedule.']}
    confirmationText={record.name ?? ''}
    confirmationLabel="Enter the database name to confirm deletion."
    shortConfirmationLabel="Database Name"
    confirmWithPassword
    onconfirm={remove}
    onclose={() => (deleting = false)}
  />
{/if}
