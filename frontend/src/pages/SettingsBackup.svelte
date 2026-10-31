<script lang="ts">
  // Port of upstream livewire/settings-backup.blade.php (App\Livewire\SettingsBackup) with
  // livewire/project/database/backup-edit/{general,s3,retention}.blade.php (BackupEdit) and the list
  // of livewire/project/database/backup-executions.blade.php (BackupExecutions).
  import Callout from '../components/Callout.svelte';
  import CopyInput from '../components/CopyInput.svelte';
  import Empty from '../components/Empty.svelte';
  import FormInput from '../components/FormInput.svelte';
  import Listbox from '../components/Listbox.svelte';
  import Reicon from '../components/Reicon.svelte';
  import SettingsLayout from '../components/SettingsLayout.svelte';
  import SettingsSection from '../components/SettingsSection.svelte';
  import StatusBadge from '../components/StatusBadge.svelte';
  import UnsavedBar from '../components/UnsavedBar.svelte';
  import { ApiError } from '../lib/api';
  import { link, router } from '../lib/router.svelte';
  import { settingsUi, type BackupPayload, type Values } from '../lib/settingsApi';
  import { notify } from '../lib/toast.svelte';

  document.title = 'Instance Backup | coolify-rs';

  let data = $state<BackupPayload | null>(null);
  let description = $state('');
  let savedDescription = $state('');
  let edit = $state<Values>({});
  let savedEdit = $state('');
  let beforeUpdate = $state(true);
  let executions = $state<Values[]>([]);
  let total = $state(0);
  let skip = $state(0);
  const take = 10;

  const onOff = [
    { value: true, label: 'Enabled' },
    { value: false, label: 'Disabled' },
  ];
  const editDirty = $derived(!!data?.backup && JSON.stringify(edit) !== savedEdit);
  const descDirty = $derived((description ?? '') !== (savedDescription ?? ''));

  function apply(d: BackupPayload) {
    data = d;
    description = d.database?.description ?? '';
    savedDescription = description;
    beforeUpdate = d.is_backup_before_update_enabled;
    if (d.backup) {
      edit = { ...d.backup.form };
      savedEdit = JSON.stringify(edit);
    }
  }

  settingsUi.backup().then((d) => {
    if (d.redirect) return router.go(d.redirect, true);
    apply(d);
    loadExecutions();
  });

  async function loadExecutions() {
    if (!data?.backup) return;
    const r = await settingsUi.backupExecutions(skip);
    executions = r.executions;
    total = r.total;
  }

  async function run(action: string, values: Values = {}) {
    try {
      const d = await settingsUi.backupAction(action, values);
      apply(d);
      if (d.message) notify.success(d.message);
      loadExecutions();
    } catch (e) {
      notify.failure(e instanceof ApiError ? e.message : String(e));
    }
  }

  const statusOf = (e: Values): [string, 'neutral' | 'success' | 'warning' | 'error'] => {
    switch (e.status) {
      case 'success':
        return e.s3_uploaded === false ? ['S3 warning', 'warning'] : ['Success', 'success'];
      case 'running':
        return ['In progress', 'warning'];
      case 'failed':
        return ['Failed', 'error'];
      default:
        return [String(e.status ?? ''), 'neutral'];
    }
  };
</script>

<div>
  <SettingsLayout>
    {#if data}
      <div class="application-settings-form mx-auto flex w-full max-w-none min-w-0 flex-col gap-6">
        {#if data.functional}
          {#if data.database && data.backup}
            <form
              onsubmit={(e) => {
                e.preventDefault();
                run('submit', { description });
              }}
            >
              <UnsavedBar dirty={descDirty} onsave={() => run('submit', { description })} onreset={() => (description = savedDescription)} />
              <SettingsSection title="Instance database">
                <div class="grid gap-4 lg:grid-cols-2">
                  <FormInput id="database-name" label="Name" readonly value={data.database.name} />
                  <FormInput id="description" label="Description" bind:value={description} />
                  <div class="lg:col-span-2">
                    <FormInput id="database-uuid" label="UUID" readonly value={data.database.uuid} />
                  </div>
                  <FormInput id="database-user" label="User" readonly value={data.database.postgres_user} />
                </div>
              </SettingsSection>
            </form>

            <form
              class="flex flex-col gap-6"
              onsubmit={(e) => {
                e.preventDefault();
                run('edit', $state.snapshot(edit));
              }}
            >
              <UnsavedBar dirty={editDirty} onsave={() => run('edit', $state.snapshot(edit))} onreset={() => (edit = JSON.parse(savedEdit))} />
              <SettingsSection title="Backup schedule" description="Choose what to back up, when it runs, and how long it may run.">
                {#snippet actions()}
                  <div class="flex items-center gap-2">
                    {#if !edit.enabled}
                      <button type="button" class="button button-highlighted" onclick={() => run('toggleEnabled')}>Enable backup</button>
                    {:else}
                      <button type="button" class="button" onclick={() => run('toggleEnabled')}>Disable backup</button>
                    {/if}
                    <button type="button" class="button" disabled={!String(data?.database?.status ?? '').startsWith('running')} onclick={() => run('backupNow')}>Back up now</button>
                  </div>
                {/snippet}
                <div class="space-y-5">
                  <div class="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
                    <FormInput label="Frequency" id="frequency" required bind:value={edit.frequency} />
                    <FormInput
                      label="Timezone"
                      id="timezone"
                      disabled
                      helper="Uses the deployment server timezone, or the instance timezone when none is configured."
                      required
                      value="Instance timezone"
                    />
                    <FormInput label="Timeout" id="timeout" type="number" min="60" helper="Maximum backup runtime in seconds." required bind:value={edit.timeout} />
                    <FormInput
                      label="Missing backup alert after"
                      id="missingBackupNotificationDays"
                      type="number"
                      min="0"
                      max="365"
                      helper="Notify through backup failure channels after this many days without an execution. Use 0 to disable."
                      required
                      bind:value={edit.missing_backup_notification_days}
                    />
                  </div>
                </div>
              </SettingsSection>

              {#if data.backup.storages.length === 0}
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
                    {#if !edit.save_s3}
                      <button type="button" class="button button-highlighted" onclick={() => run('toggleS3', $state.snapshot(edit))}>Enable S3</button>
                    {:else}
                      <button type="button" class="button" onclick={() => run('toggleS3', $state.snapshot(edit))}>Disable S3</button>
                    {/if}
                  {/snippet}
                  <div class="grid gap-4 sm:grid-cols-2">
                    <div class="flex flex-col gap-1.5">
                      <!-- svelte-ignore a11y_label_has_associated_control -->
                      <label class="flex gap-1 items-center mb-1 text-sm font-medium text-black dark:text-white">S3 storages</label>
                      {#each data.backup.storages as s}
                        <label class="flex items-center gap-2 text-sm">
                          <input
                            type="checkbox"
                            checked={(edit.s3_storage_ids ?? []).includes(s.id)}
                            onchange={(e) => {
                              const on = e.currentTarget.checked;
                              edit.s3_storage_ids = on ? [...edit.s3_storage_ids, s.id] : edit.s3_storage_ids.filter((x: number) => x !== s.id);
                              run('edit', $state.snapshot(edit));
                            }}
                          />
                          {s.name}{s.is_usable ? '' : ' (unavailable)'}
                        </label>
                      {/each}
                    </div>
                    <Listbox
                      id="disableLocalBackup"
                      label="Local copy"
                      disabled={!edit.save_s3}
                      bind:value={edit.disable_local_backup}
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
                      <FormInput label="Backups to keep" id="retention-amount-local" type="number" min="0" helper="Maximum number of recent local backups." required bind:value={edit.database_backup_retention_amount_locally} />
                      <FormInput label="Days to keep" id="retention-days-local" type="number" min="0" helper="Remove local backups older than this many days." required bind:value={edit.database_backup_retention_days_locally} />
                      <FormInput label="Maximum storage (GB)" id="retention-storage-local" type="number" min="0" step="any" helper="Remove oldest local backups after this total size is reached." required bind:value={edit.database_backup_retention_max_storage_locally} />
                    </div>
                  </div>
                  <div class="border-t border-neutral-200 pt-6 dark:border-white/[0.06]">
                    <h3 class="mb-3 text-sm font-semibold text-black dark:text-fg">S3 backups</h3>
                    <div class="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
                      <FormInput label="Backups to keep" id="retention-amount-s3" type="number" min="0" helper="Maximum number of recent S3 backups." required bind:value={edit.database_backup_retention_amount_s3} />
                      <FormInput label="Days to keep" id="retention-days-s3" type="number" min="0" helper="Remove S3 backups older than this many days." required bind:value={edit.database_backup_retention_days_s3} />
                      <FormInput label="Maximum storage (GB)" id="retention-storage-s3" type="number" min="0" step="any" helper="Remove oldest S3 backups after this total size is reached." required bind:value={edit.database_backup_retention_max_storage_s3} />
                    </div>
                  </div>
                </div>
              </SettingsSection>
            </form>

            <SettingsSection title="Backup before update" description="Back up the Coolify database before an update is installed. If the backup fails, the update is not installed.">
              <div class="max-w-md">
                <Listbox
                  id="is_backup_before_update_enabled"
                  label="Database backup"
                  helper="Runs this backup once before every manual or automatic update, in addition to the schedule."
                  bind:value={beforeUpdate}
                  onchange={(v) => run('instant', { is_backup_before_update_enabled: v })}
                  options={onOff}
                />
              </div>
            </SettingsSection>

            <section class="application-settings-section overflow-hidden">
              <div class="application-settings-section-header">
                <div><h2>Executions</h2></div>
              </div>
              <div class="application-settings-section-body p-0!">
                {#if total === 0}
                  <div class="p-4">
                    <Empty size="sm" title="No backup executions" description="Execution history appears here after the schedule runs." iconName="browser-terminal" />
                  </div>
                {:else}
                  <div class="data-table deployment-table-scroll backup-executions-table-scroll">
                    <div class="data-table-header backup-executions-table-grid h-auto rounded-none px-4 py-2.5 text-[11px]">
                      <span>Status</span><span>Database</span><span>Archive</span><span>Time</span><span>Size</span><span>Availability</span><span class="text-right">Actions</span>
                    </div>
                    {#each executions as e (e.id)}
                      {@const st = statusOf(e)}
                      <div class="backup-execution-row">
                        <div class="data-table-row backup-executions-table-grid min-h-14 border-b border-neutral-200 px-4 py-2.5 dark:border-white/[0.06]">
                          <div class="flex items-center gap-2"><StatusBadge label={st[0]} type={st[1]} /></div>
                          <div class="truncate text-[12px] font-medium text-black dark:text-fg">{e.database_name ?? 'N/A'}</div>
                          <div class="min-w-0"><CopyInput text={e.filename ?? 'No archive name'} /></div>
                          <div class="text-[11px] text-neutral-500 dark:text-fg-faint">{e.finished_at ?? e.updated_at}</div>
                          <div class="tabular-nums text-neutral-500 dark:text-fg-dim">{e.size ?? '-'}</div>
                          <div class="flex flex-wrap items-center gap-1.5">
                            <StatusBadge label={e.local_storage_deleted ? 'Local deleted' : 'Local'} type={e.local_storage_deleted ? 'neutral' : 'success'} />
                          </div>
                          <div class="flex items-center justify-end gap-1"></div>
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
                          loadExecutions();
                        }}><Reicon name="arrow-right" class="size-3.5 rotate-180" /></button
                      >
                      <button
                        type="button"
                        class="icon-button"
                        disabled={skip + take >= total}
                        aria-label="Next page"
                        onclick={() => {
                          skip += take;
                          loadExecutions();
                        }}><Reicon name="arrow-right" class="size-3.5" /></button
                      >
                    </div>
                  </div>
                {/if}
              </div>
            </section>
          {:else}
            <SettingsSection title="Instance backup">
              <Empty title="Backup is not configured" description="Coolify needs an internal database resource to create automatic backups." iconName="database" size="sm">
                {#snippet contents()}
                  <button type="button" class="button button-highlighted" onclick={() => run('configure')}>Configure backup</button>
                {/snippet}
              </Empty>
            </SettingsSection>
          {/if}
        {:else}
          <SettingsSection title="Instance backup">
            <Callout type="danger" title="Localhost is not ready">
              Validate the localhost connection before configuring instance backups.
              <a href="/server/{data.server_uuid}" use:link class="font-medium underline">Open server settings</a>
            </Callout>
          </SettingsSection>
        {/if}
      </div>
    {/if}
  </SettingsLayout>
</div>
