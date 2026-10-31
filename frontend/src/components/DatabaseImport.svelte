<script lang="ts">
  // Port of upstream livewire/project/database/import.blade.php (Project/Database/Import) and
  // import-form.blade.php (Project/Database/ImportForm). Files go up in Dropzone's chunk format.
  import { untrack } from 'svelte';
  import Callout from './Callout.svelte';
  import Checkbox from './Checkbox.svelte';
  import ConfirmModal from './ConfirmModal.svelte';
  import Empty from './Empty.svelte';
  import FormInput from './FormInput.svelte';
  import FormTextarea from './FormTextarea.svelte';
  import Listbox from './Listbox.svelte';
  import ProcessDialog from './ProcessDialog.svelte';
  import Reicon from './Reicon.svelte';
  import SettingsSection from './SettingsSection.svelte';
  import { ApiError, type DatabaseRecord } from '../lib/api';
  import { importUi, type ImportFormState } from '../lib/databaseApi';
  import { notify } from '../lib/toast.svelte';

  let { record, canUpdate }: { record: DatabaseRecord; canUpdate: boolean } = $props();

  let data = $state<ImportFormState | null>(null);
  let dumpAll = $state(false);
  let replaceExisting = $state(false);
  let keepOwners = $state(false);
  let restoreMysqlUsers = $state(false);
  let sqliteDatabase = $state('');
  let restoreCommand = $state('');
  let restoreType = $state<'file' | 's3' | null>(null);

  let customLocation = $state('');
  let fileSource = $state<'upload' | 'server' | null>(null);
  let uploadId = $state<string | null>(null);
  let filename = $state<string | null>(null);
  let filesize = $state<string | null>(null);
  let isUploading = $state(false);
  let progress = $state(0);
  let uploadFailed = $state(false);
  let dragging = $state(false);
  let picker: HTMLInputElement | undefined = $state();

  let s3StorageId = $state('');
  let s3Path = $state('');
  let s3FileSize = $state<number | null>(null);
  let s3SizeText = $state('');
  let s3Uuid = $state<string | null>(null);

  let confirm = $state<'file' | 's3' | null>(null);
  let monitor = $state(false);
  let activityId = $state<number | null>(null);
  let activity = $state<{ status: string; output: string | null } | null>(null);

  async function load() {
    data = await importUi.form(record.uuid);
    restoreCommand = data.restore_command;
    if (data.sqlite_files.length && !sqliteDatabase) sqliteDatabase = data.sqlite_files[0];
  }
  $effect(() => {
    untrack(() => load());
    // `refreshStatus` on `DatabaseStatusChanged`.
    const poll = setInterval(() => importUi.form(record.uuid).then((d) => data && (data.status = d.status)).catch(() => {}), 10000);
    return () => clearInterval(poll);
  });

  const type = $derived(data?.db_type ?? '');
  const options = () => ({
    dump_all: dumpAll,
    replace_existing: replaceExisting,
    keep_owners: keepOwners,
    restore_mysql_users: restoreMysqlUsers,
    ...(type === 'standalone-sqlite' ? { sqlite_database: sqliteDatabase } : {}),
  });
  async function refreshCommand() {
    restoreCommand = (await importUi.command(record.uuid, options())).restore_command;
  }

  /** `selectSqliteDatabaseFor`: `defaultRestoreFile`. */
  function selectSqliteFor(name: string | null) {
    if (type !== 'standalone-sqlite' || !data) return;
    const base = (name ?? '').split('/').pop() ?? '';
    const hit = [...data.sqlite_files].sort((a, b) => b.length - a.length).find((f) => base === f || base.startsWith(`${f}.`) || base.startsWith(`sqlite-backup-${f}-`));
    sqliteDatabase = hit ?? data.sqlite_files[0] ?? '';
    refreshCommand();
  }

  async function checkFile() {
    if (!customLocation) return;
    const r = await importUi.checkFile(record.uuid, customLocation);
    if (r.exists) {
      filename = customLocation;
      filesize = null;
      fileSource = 'server';
      uploadId = null;
      uploadFailed = false;
      selectSqliteFor(customLocation);
    }
  }

  async function upload(file: File) {
    const chunkSize = 10_000_000;
    const id = crypto.randomUUID();
    const count = Math.max(1, Math.ceil(file.size / chunkSize));
    isUploading = true;
    uploadFailed = false;
    customLocation = '';
    progress = 0;
    filename = file.name;
    fileSource = null;
    uploadId = null;
    try {
      for (let index = 0; index < count; index++) {
        const form = new FormData();
        form.append('upload_id', id);
        form.append('dzchunkindex', String(index));
        form.append('dztotalchunkcount', String(count));
        form.append('dztotalfilesize', String(file.size));
        form.append('dzTotalFilesize', String(file.size));
        form.append('file', file.slice(index * chunkSize, (index + 1) * chunkSize), file.name);
        await importUi.uploadChunk(record.uuid, form);
        progress = Math.round(((index + 1) / count) * 100);
      }
      uploadId = id;
      fileSource = 'upload';
      filesize = `${(file.size / 1024 / 1024).toFixed(2)} MB`;
      selectSqliteFor(file.name);
    } catch (e) {
      uploadFailed = true;
      notify.failure(e instanceof Error ? e.message : String(e));
    } finally {
      isUploading = false;
    }
  }

  async function checkS3() {
    s3FileSize = null;
    const path = s3Path.trim();
    if (path && !path.startsWith('/')) s3Path = `/${path}`;
    const r = await importUi.checkS3(record.uuid, s3StorageId ? Number(s3StorageId) : null, s3Path);
    if (r.size !== undefined) {
      s3FileSize = r.size;
      s3SizeText = r.size_text ?? '';
      s3Uuid = r.s3_storage_uuid ?? null;
      selectSqliteFor(s3Path);
    }
  }

  async function run(password: string, source: 'file' | 's3') {
    let body: Record<string, unknown>;
    if (source === 's3') {
      body = { source: 's3', s3_storage_uuid: s3Uuid, s3_storage_id: Number(s3StorageId), path: s3Path, ...options(), password };
    } else if (fileSource === 'upload') {
      body = { source: 'upload', upload_id: uploadId, ...options(), password };
    } else {
      body = { source: fileSource ?? '', path: customLocation, ...options(), password };
    }
    try {
      const r = await importUi.run(record.uuid, body);
      if (source === 'file') {
        filename = null;
        fileSource = null;
        uploadId = null;
      }
      if (r.activity_id !== undefined) {
        activityId = r.activity_id;
        activity = null;
        monitor = true;
      }
    } catch (e) {
      if (e instanceof ApiError && e.message === 'The provided password is incorrect.') return e.message;
      notify.failure(e instanceof ApiError ? e.message : String(e));
    }
  }

  // `activity-monitor`: follows the import until it ends.
  $effect(() => {
    if (activityId === null || !monitor) return;
    const id = activityId;
    let stop = false;
    const tick = async () => {
      try {
        activity = await importUi.activity(record.uuid, id);
      } catch {
        /* the next tick retries */
      }
      if (!stop && !['finished', 'error', 'killed', 'cancelled', 'closed', 'failed'].includes(activity?.status ?? '')) setTimeout(tick, 1000);
    };
    tick();
    return () => (stop = true);
  });
  const chosen = 'border-coollabs/35 bg-coollabs/[0.06] text-coollabs dark:border-brand-light/30 dark:bg-brand/[0.08] dark:text-brand-light';
  const idle = 'border-neutral-200 bg-white hover:border-neutral-300 dark:border-white/[0.08] dark:bg-white/[0.05] dark:hover:border-white/[0.14]';
</script>

<div class="application-settings-form">
  {#if data}
    {#if data.unsupported}
      <SettingsSection title="Restore database" description="Import a backup into this database.">
        <Empty title="Restore is not supported" description="This database type does not currently support backup imports." iconName="database" size="sm" />
      </SettingsSection>
    {:else if !data.status.startsWith('running')}
      <SettingsSection title="Restore database" description="Import a backup into this database.">
        <Empty title="Start the database first" description="The database must be running before Coolify can restore a backup." iconName="database" size="sm" />
      </SettingsSection>
    {:else}
      <div class="application-settings-workspace flex flex-col gap-6">
        <Callout type="danger" title="Restoring a backup changes database data">
          Review the source and import command before continuing. Existing objects can cause the import to fail unless replacement is enabled.
        </Callout>

        <SettingsSection title="Restore configuration" description="Configure how the selected backup is applied to this database.">
          <div class="space-y-4">
            {#if type === 'standalone-postgresql' && dumpAll}
              <Callout type="warning" title="Full restore runs as a superuser">
                Restoring all databases needs a PostgreSQL superuser (it recreates databases and roles), so Coolify cannot restrict it. Only use a backup you trust.
                <span class="mt-1 block">
                  The backup also replaces PostgreSQL administrator role passwords, including the destination administrator password. If the administrator password in the backup differs from the one in Coolify, later backups and connections can fail until you synchronize them.
                </span>
              </Callout>
            {/if}
            <FormTextarea
              id="restoreCommandText"
              rows={10}
              readonly
              label="Import command"
              helper="Coolify detects the backup format (SQL, archive, gzip, bz2, xz, zip, or tar) before it changes the database."
              value={restoreCommand}
            />
            {#if type !== 'standalone-sqlite'}
              <div class="max-w-sm">
                <Listbox
                  id="dumpAll"
                  label="Backup contents"
                  bind:value={dumpAll}
                  onchange={refreshCommand}
                  options={[
                    { value: true, label: 'Backup contains all databases' },
                    { value: false, label: 'Backup contains one database' },
                  ]}
                />
              </div>
            {/if}
            {#if type === 'standalone-postgresql' && !dumpAll}
              <div class="max-w-sm">
                <Checkbox
                  id="replaceExisting"
                  label="Replace objects that already exist"
                  helper="Archive backups: drops matching tables, functions, types, and other PostgreSQL objects before restoring them. SQL backups: recreates the database before restoring it, so all existing data in it is removed."
                  disabled={!canUpdate}
                  bind:checked={replaceExisting}
                  onchange={refreshCommand}
                />
              </div>
              <div class="max-w-sm">
                <Checkbox
                  id="keepOwners"
                  label="Keep owners and privileges"
                  helper="Archive backups: restores object owners and GRANTs from the backup. Leave this off for backups from another server (for example Amazon RDS): its roles do not exist here, and the restore would fail."
                  disabled={!canUpdate}
                  bind:checked={keepOwners}
                  onchange={refreshCommand}
                />
              </div>
            {/if}
            {#if (type === 'standalone-mysql' || type === 'standalone-mariadb') && dumpAll}
              <div class="max-w-sm">
                <Checkbox
                  id="restoreMysqlUsers"
                  label="Restore users and privileges (mysql system database)"
                  helper="Off: the system databases (mysql, sys) of the backup are skipped, so this database keeps its own users and passwords. On: the users, passwords, and privileges of the backup replace those of this database."
                  disabled={!canUpdate}
                  bind:checked={restoreMysqlUsers}
                  onchange={refreshCommand}
                />
              </div>
              {#if restoreMysqlUsers}
                <Callout type="warning" title="Restoring users changes passwords">
                  The backup replaces all users, passwords, and privileges, including the root password. After the next restart of the database, the passwords from the backup apply.
                  <span class="mt-1 block">Update the passwords in Coolify's database configuration after the restore.</span>
                </Callout>
              {/if}
            {/if}
            {#if type === 'standalone-sqlite' && data.sqlite_files.length}
              <div class="max-w-sm">
                <Listbox
                  id="sqliteDatabase"
                  label="Restore into"
                  helper="The database file the backup replaces. Coolify preselects the file named in the backup file name."
                  disabled={!canUpdate}
                  bind:value={sqliteDatabase}
                  onchange={refreshCommand}
                  options={data.sqlite_files.map((f) => ({ value: f, label: f }))}
                />
              </div>
            {/if}
            {#if type === 'standalone-mongodb'}
              <div class="max-w-sm">
                <Checkbox
                  id="replaceExisting"
                  label="Replace collections that already exist"
                  helper="Drops each collection from the backup before restoring it. Without this option, documents that already exist are skipped."
                  disabled={!canUpdate}
                  bind:checked={replaceExisting}
                  onchange={refreshCommand}
                />
              </div>
            {/if}
          </div>
        </SettingsSection>

        <SettingsSection title="Backup source" description="Choose a local file or an object stored in S3.">
          <div class="grid gap-3 sm:grid-cols-2">
            <button type="button" onclick={() => (restoreType = 'file')} class="flex min-h-20 items-center gap-3 rounded-[10px] border p-3 text-left transition-colors {restoreType === 'file' ? chosen : idle}">
              <span class="flex size-9 shrink-0 items-center justify-center rounded-lg bg-neutral-100 dark:bg-white/[0.06]">
                <Reicon name="file" class="size-4" />
              </span>
              <span>
                <span class="block text-[13px] font-semibold">File</span>
                <span class="mt-0.5 block text-[11px] text-neutral-500 dark:text-fg-faint">Upload a backup or use a server path.</span>
              </span>
            </button>
            {#if data.storages.length}
              <button type="button" onclick={() => (restoreType = 's3')} class="flex min-h-20 items-center gap-3 rounded-[10px] border p-3 text-left transition-colors {restoreType === 's3' ? chosen : idle}">
                <span class="flex size-9 shrink-0 items-center justify-center rounded-lg bg-neutral-100 dark:bg-white/[0.06]">
                  <Reicon name="storages" class="size-4" />
                </span>
                <span>
                  <span class="block text-[13px] font-semibold">S3 storage</span>
                  <span class="mt-0.5 block text-[11px] text-neutral-500 dark:text-fg-faint">Download a backup from an S3 bucket.</span>
                </span>
              </button>
            {/if}
          </div>

          {#if canUpdate && restoreType === 'file'}
            <div class="mt-4 rounded-[10px] border border-neutral-200 bg-neutral-50 p-4 dark:border-white/[0.08] dark:bg-white/[0.05]">
              <form
                class="flex flex-col gap-3 sm:flex-row sm:items-end"
                onsubmit={(e) => {
                  e.preventDefault();
                  checkFile();
                }}
              >
                <div class="min-w-0 flex-1">
                  <FormInput id="customLocation" label="File path on the server" placeholder="/home/user/backup.sql.gz" bind:value={customLocation} oninput={() => fileSource === 'server' && (fileSource = null)} />
                </div>
                <button type="submit" class="button" disabled={!customLocation}>Check file</button>
              </form>

              <div class="my-4 flex items-center gap-3 text-[10px] font-medium uppercase tracking-wide text-neutral-400 dark:text-fg-faint">
                <span class="h-px flex-1 bg-neutral-200 dark:bg-white/[0.08]"></span>
                or upload
                <span class="h-px flex-1 bg-neutral-200 dark:bg-white/[0.08]"></span>
              </div>

              <!-- svelte-ignore a11y_no_static_element_interactions -->
              <div
                id="my-dropzone"
                class="dropzone dz-clickable rounded-lg! border! border-dashed! border-neutral-300! bg-white! dark:border-white/[0.12]! dark:bg-white/[0.05]! {dragging ? 'dz-drag-hover' : ''}"
                ondragover={(e) => {
                  e.preventDefault();
                  dragging = true;
                }}
                ondragleave={() => (dragging = false)}
                ondrop={(e) => {
                  e.preventDefault();
                  dragging = false;
                  const file = e.dataTransfer?.files?.[0];
                  if (file) upload(file);
                }}
              >
                <div class="dz-default dz-message">
                  <button class="dz-button" type="button" onclick={() => picker?.click()}>Select or drop a backup file here.</button>
                </div>
                <input
                  bind:this={picker}
                  type="file"
                  class="hidden"
                  onchange={(e) => {
                    const file = e.currentTarget.files?.[0];
                    if (file) upload(file);
                    e.currentTarget.value = '';
                  }}
                />
              </div>
              {#if isUploading}
                <div class="mt-3 h-1.5 overflow-hidden rounded-full bg-neutral-200 dark:bg-white/[0.08]">
                  <div class="h-full rounded-full bg-coollabs dark:bg-brand" style="width: {progress}%"></div>
                </div>
              {/if}

              {#if filename && !uploadFailed && !isUploading}
                <div class="mt-4 flex flex-col gap-3 rounded-lg border border-neutral-200 bg-white p-3 dark:border-white/[0.08] dark:bg-white/[0.03] sm:flex-row sm:items-center sm:justify-between">
                  <div class="min-w-0">
                    <p class="truncate text-[12px] font-medium">{filename ?? 'N/A'}</p>
                    {#if filesize}<p class="mt-0.5 text-[11px] text-neutral-500 dark:text-fg-faint">{filesize}</p>{/if}
                  </div>
                  <div class="shrink-0">
                    <button type="button" class="button button-error" onclick={() => (confirm = 'file')}>Restore from file</button>
                  </div>
                </div>
              {/if}
            </div>
          {/if}

          {#if canUpdate && data.storages.length && restoreType === 's3'}
            <div class="mt-4 rounded-[10px] border border-neutral-200 bg-neutral-50 p-4 dark:border-white/[0.08] dark:bg-white/[0.05]">
              <div class="grid gap-4 sm:grid-cols-2">
                <Listbox
                  id="s3StorageId"
                  label="S3 storage"
                  placeholder="Select storage"
                  bind:value={s3StorageId}
                  onchange={() => (s3FileSize = null)}
                  options={data.storages.map((s) => ({ value: String(s.id), label: s.name + (s.description ? ` - ${s.description}` : '') }))}
                />
                <FormInput
                  id="s3Path"
                  label="File path"
                  helper="Path to the backup file in your S3 bucket, e.g., /backups/database-2025-01-15.gz"
                  placeholder="/backups/database-backup.gz"
                  bind:value={s3Path}
                  oninput={() => (s3FileSize = null)}
                  onkeydown={(e: KeyboardEvent) => {
                    if (e.key === 'Enter') {
                      e.preventDefault();
                      checkS3();
                    }
                  }}
                />
              </div>
              <div class="mt-3 flex justify-end">
                <button type="button" class="button" disabled={!s3StorageId || !s3Path} onclick={checkS3}>Check file</button>
              </div>
              {#if s3FileSize}
                <div class="mt-4 flex flex-col gap-3 rounded-lg border border-neutral-200 bg-white p-3 dark:border-white/[0.08] dark:bg-white/[0.03] sm:flex-row sm:items-center sm:justify-between">
                  <div class="min-w-0">
                    <p class="truncate text-[12px] font-medium">{s3Path}</p>
                    <p class="mt-0.5 text-[11px] text-neutral-500 dark:text-fg-faint">{s3SizeText}</p>
                  </div>
                  <div class="shrink-0">
                    <button type="button" class="button button-error" onclick={() => (confirm = 's3')}>Restore from S3</button>
                  </div>
                </div>
              {/if}
            </div>
          {/if}
        </SettingsSection>
      </div>
    {/if}
  {/if}
</div>

{#if confirm === 'file'}
  <ConfirmModal
    title="Restore Database from File?"
    confirmWithPassword
    onconfirm={(password) => run(password, 'file')}
    onclose={() => (confirm = null)}
  />
{:else if confirm === 's3'}
  <ConfirmModal
    title="Restore Database from S3?"
    confirmWithPassword
    onconfirm={(password) => run(password, 's3')}
    onclose={() => (confirm = null)}
  />
{/if}

{#if monitor}
  <ProcessDialog title="Database Restore Output" closeWithX size="xl" onclose={() => (monitor = false)}>
    <div class="flex h-full min-h-0 flex-col">
      <pre class="h-full min-h-0 flex-1 overflow-auto whitespace-pre-wrap p-4 font-mono text-xs">{activity?.output ?? ''}</pre>
    </div>
  </ProcessDialog>
{/if}
