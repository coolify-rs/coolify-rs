<script lang="ts">
  // Port of upstream livewire/project/service/storage.blade.php and
  // livewire/project/shared/storages/all.blade.php (Project\Service\Storage, Project\Shared\Storages\All).
  // External Compose volumes and the stale-entry actions of Compose volumes are not ported.
  import Reicon from '../Reicon.svelte';
  import RowActions from '../RowActions.svelte';
  import RowAction from '../RowAction.svelte';
  import Modal from '../Modal.svelte';
  import Callout from '../Callout.svelte';
  import Dropdown from '../Dropdown.svelte';
  import Empty from '../Empty.svelte';
  import FormInput from '../FormInput.svelte';
  import FormTextarea from '../FormTextarea.svelte';
  import Helper from '../Helper.svelte';
  import Listbox from '../Listbox.svelte';
  import ConfirmModal from '../ConfirmModal.svelte';
  import SettingsSection from '../SettingsSection.svelte';
  import FileStorage from './FileStorage.svelte';
  import BackupTargetForm from '../BackupTargetForm.svelte';
  import NewScheduledBackup from '../NewScheduledBackup.svelte';
  import { link, router } from '../../lib/router.svelte';
  import { volumeBackupsUi, type BackupOwner, type BackupTargetOption } from '../../lib/volumeBackupsApi';
  import { ApiError, request } from '../../lib/api';
  import { resPath, type ResourceKind } from '../../lib/sharedApi';
  import { notify } from '../../lib/toast.svelte';

  // `volumesOnly`: just `storages/all.blade.php`, for service storage cards that render their own section and tabs.
  let { kind, uuid, onchange, volumesOnly = false }: { kind: ResourceKind; uuid: string; onchange?: () => void; volumesOnly?: boolean } = $props();

  interface Volume {
    id: number;
    name: string;
    mount_path: string;
    host_path: string | null;
    is_preview_suffix_enabled: boolean;
    is_read_only: boolean;
    is_shared: boolean;
  }

  let data = $state<any>(null);
  let forms = $state<Record<number, Volume>>({});
  let activeTab = $state<'volumes' | 'files' | 'directories'>('volumes');
  let tabChosen = false;
  let modal = $state<'volume' | 'file' | 'host-file' | 'directory' | null>(null);
  let error = $state<ApiError | null>(null);
  let deleting = $state<Volume | null>(null);
  /** `$pendingRename`: a volume rename that waits for confirmation. A new name mounts another Docker volume, and the data stays in the old one. */
  let pendingRename = $state<{ storageId: number; from: string; to: string } | null>(null);
  let backupModal = $state<number | null>(null);
  let databaseBackupOpen = $state(false);
  let backupTargets = $state<BackupTargetOption[]>([]);
  const backupOwner = $derived<BackupOwner>(kind === 'application' ? 'application' : 'service');
  const backupOwnerUuid = $derived<string>(kind === 'application' ? uuid : (data?.service_uuid ?? ''));

  /** The `x-modal-input` of the Backup button: `Application/Backup/Create` or `Service/VolumeBackup/Create`. */
  async function openBackup(id: number) {
    backupTargets = (await volumeBackupsUi.index(backupOwner, backupOwnerUuid)).targets;
    backupModal = id;
  }

  let name = $state('');
  let mountPath = $state('');
  let filePath = $state('');
  let fileSource = $state('');
  let fileContent = $state('');
  let hostSource = $state('');
  let hostDestination = $state('');
  let dirSource = $state('');
  let dirDestination = $state('');

  const base = $derived(`${resPath(kind, uuid)}/storages`);
  const volumes = $derived<Volume[]>(data?.volumes ?? []);
  const files = $derived<any[]>((data?.files ?? []).filter((f: any) => !f.is_directory));
  const directories = $derived<any[]>((data?.files ?? []).filter((f: any) => f.is_directory));
  const hasAny = $derived(volumes.length > 0 || files.length > 0 || directories.length > 0);
  const canUpdate = $derived(data?.can_update ?? false);
  const hasSourcePaths = $derived(volumes.some((v) => !!v.host_path));
  const gridClass = $derived(data?.supports_preview_suffix ? 'volumes-table-grid-with-pr' : canUpdate ? 'volumes-table-grid' : 'volumes-table-grid-readonly');
  const hostPath = $derived<string>(data?.host_path ?? '');
  const previewPath = $derived.by(() => {
    const source = fileSource.trim();
    if (source !== '') return source.startsWith('/') ? source : `${hostPath}/${source.replace(/^\.\//, '')}`;
    const path = filePath.trim();
    return hostPath + (path === '' ? '/' : path.startsWith('/') ? path : `/${path}`);
  });

  const tabBase = 'h-7 rounded-md px-2.5 text-[12px] font-medium transition-colors disabled:cursor-not-allowed disabled:opacity-40';
  const tabActive = 'bg-white text-black shadow-sm ring-1 ring-neutral-200 dark:bg-white/[0.09] dark:text-fg dark:ring-white/[0.08]';
  const tabInactive = 'text-neutral-500 hover:text-black dark:text-fg-faint dark:hover:text-fg';

  async function load() {
    const d = await request<any>('GET', base);
    data = d;
    pendingRename = null;
    forms = Object.fromEntries(d.volumes.map((v: Volume) => [v.id, { ...v }]));
    if (!tabChosen) {
      // `resolveDefaultTab`.
      const f = d.files.filter((x: any) => !x.is_directory).length;
      const dirs = d.files.length - f;
      activeTab = d.volumes.length > 0 ? 'volumes' : f > 0 ? 'files' : dirs > 0 ? 'directories' : 'volumes';
    }
  }

  $effect(() => {
    load();
  });

  function clearForm() {
    name = data?.default_volume_name ?? '';
    mountPath = '';
    filePath = '';
    fileSource = '';
    fileContent = '';
    hostSource = '';
    hostDestination = '';
    dirSource = hostPath;
    dirDestination = '';
    error = null;
  }

  function open(which: typeof modal) {
    clearForm();
    modal = which;
  }

  async function submitNew(e: Event, path: string, body: unknown, tab: typeof activeTab) {
    e.preventDefault();
    error = null;
    try {
      const r = await request<{ message: string }>('POST', `${base}/${path}`, body);
      notify.success(r.message);
      modal = null;
      activeTab = tab;
      tabChosen = true;
      await load();
      onchange?.();
    } catch (err) {
      const f = err instanceof ApiError ? err : new ApiError(0, String(err));
      // `handleError`: a validation failure of these actions is a toast.
      notify.failure(f.details ?? f.message);
    }
  }

  // `cancelRename($storageId)`.
  function cancelRename() {
    if (!pendingRename) return;
    const stored = volumes.find((v) => v.id === pendingRename?.storageId);
    if (stored && forms[stored.id]) forms[stored.id].name = stored.name;
    pendingRename = null;
  }

  async function saveVolume(id: number, e?: Event, renameConfirmed = false) {
    e?.preventDefault();
    const f = forms[id];
    // `submit($storageId, $renameConfirmed)`: a rename waits for confirmation.
    const stored = volumes.find((v) => v.id === id);
    if (stored && !stored.is_shared && f.name !== stored.name && !renameConfirmed) {
      pendingRename = { storageId: id, from: stored.name, to: f.name };
      return;
    }
    pendingRename = null;
    try {
      const r = await request<{ message: string }>('PATCH', `${base}/volume/${id}`, {
        name: f.name,
        mount_path: f.mount_path,
        is_preview_suffix_enabled: f.is_preview_suffix_enabled,
      });
      notify.success(r.message);
      await load();
    } catch (err) {
      notify.failure(err instanceof ApiError ? err.message : String(err));
    }
  }
</script>

{#snippet backupCell(id: number)}
  {@const meta = data?.volume_backup_meta?.[id]}
  {#if meta?.enabled}
    {#if meta.url}
      <a use:link href={meta.url} class="table-badge underline-offset-2 hover:underline {meta.s3 ? 'table-badge-success' : ''}" title="Volume backup is enabled" aria-label={meta.s3 ? 'Backups are saved to S3' : 'Backups are stored locally only'}>{meta.s3 ? 'S3' : 'Local'}</a>
    {:else}
      <span class="table-badge {meta.s3 ? 'table-badge-success' : ''}" title="Volume backup is enabled" aria-label={meta.s3 ? 'Backups are saved to S3' : 'Backups are stored locally only'}>{meta.s3 ? 'S3' : 'Local'}</span>
    {/if}
  {:else}
    <span class="data-table-cell-dash">-</span>
  {/if}
{/snippet}

{#snippet tabs()}
  {#if hasAny}
    <div class="inline-flex items-center gap-0.5 rounded-lg bg-neutral-100 p-1 dark:bg-white/[0.04]">
      <button type="button" onclick={() => ((activeTab = 'volumes'), (tabChosen = true))} disabled={volumes.length === 0} class="{tabBase} {activeTab === 'volumes' ? tabActive : tabInactive}">Volumes ({volumes.length})</button>
      <button type="button" onclick={() => ((activeTab = 'files'), (tabChosen = true))} disabled={files.length === 0} class="{tabBase} {activeTab === 'files' ? tabActive : tabInactive}">Files ({files.length})</button>
      <button type="button" onclick={() => ((activeTab = 'directories'), (tabChosen = true))} disabled={directories.length === 0} class="{tabBase} {activeTab === 'directories' ? tabActive : tabInactive}">Directories ({directories.length})</button>
    </div>
  {/if}
{/snippet}

{#snippet prHelper()}
  <Helper helper="Adds -pr-N to the storage name or path so each preview uses isolated data. Disabling it shares production data with previews." />
{/snippet}

{#snippet volumeTable()}
  <div class="flex w-full flex-col">
    {#if data.build_pack === 'dockercompose'}
      <div class="border-b border-neutral-200 px-4 py-3 text-[13px] leading-5 text-amber-800 dark:border-white/[0.08] dark:text-amber-300/90">
        Docker Compose volume mounts are read-only here. Edit the compose file and reload it to change volumes.
      </div>
    {/if}
    <div class="data-table w-full">
      <div class="data-table-header {gridClass} {hasSourcePaths ? 'has-source' : ''}">
        <span>Storage Name</span>
        {#if hasSourcePaths}<span>Source Path</span>{/if}
        <span>Destination Path</span>
        {#if data.supports_preview_suffix}
          <div class="volumes-col-pr flex items-center gap-1.5">
            <span>PR suffix</span>
            {@render prHelper()}
          </div>
        {/if}
        <span class="volumes-col-backup text-center">Backup</span>
        {#if canUpdate}<span class="volumes-col-actions text-right">Actions</span>{/if}
      </div>
      {#each volumes as v (v.id)}
        {@const f = forms[v.id]}
        {#if f && v.is_read_only}
          <div class="env-table-item">
            <div class="data-table-row {gridClass} {hasSourcePaths ? 'has-source' : ''} text-[13px] text-neutral-700 dark:text-fg-dim">
              <div class="volumes-cell-name min-w-0">
                <span class="volumes-mobile-label volumes-field-label">Storage Name</span>
                <div class="flex min-w-0 items-center gap-2">
                  <span class="min-w-0 truncate text-[13px] font-medium text-neutral-950 dark:text-fg" title={v.name}>{v.name}</span>
                </div>
              </div>
              {#if hasSourcePaths}
                <div class="volumes-cell-source min-w-0">
                  <span class="volumes-mobile-label volumes-field-label">Source Path</span>
                  {#if v.host_path}<FormInput id="source-{v.id}" aria-label="Source Path" value={v.host_path} readonly />{:else}<span class="data-table-cell-dash">-</span>{/if}
                </div>
              {/if}
              <div class="volumes-cell-dest min-w-0">
                <span class="volumes-mobile-label volumes-field-label">Destination Path</span>
                <span class="block min-w-0 truncate text-[13px] text-neutral-950 dark:text-fg" title={v.mount_path}>{v.mount_path}</span>
              </div>
              {#if data.supports_preview_suffix}
                <div class="volumes-col-pr min-w-0">
                  <div class="volumes-mobile-label volumes-field-label flex items-center gap-1.5"><span>PR suffix</span>{@render prHelper()}</div>
                  <span>{v.is_preview_suffix_enabled ? 'Add suffix' : 'Share volume'}</span>
                </div>
              {/if}
              <div class="volumes-col-backup flex items-center justify-center gap-1.5">
                <span class="volumes-mobile-label volumes-field-label">Backup</span>
                {@render backupCell(v.id)}
              </div>
              {#if data.show_backup_action}
                <div class="volumes-col-actions volumes-cell-actions flex flex-nowrap items-center justify-end gap-1.5">
                  {#if canUpdate}
                    <button type="button" class="icon-button shrink-0" title="Configure volume backup" aria-label="Configure backup for {v.name}" onclick={() => openBackup(v.id)}>
                      <Reicon name="database" class="size-3.5" />
                    </button>
                  {:else}
                    <span class="text-neutral-400 dark:text-fg-faint">—</span>
                  {/if}
                </div>
              {/if}
            </div>
          </div>
        {:else if f}
          <form class="env-table-item" onsubmit={(e) => saveVolume(v.id, e)}>
            <div class="data-table-row {gridClass} {hasSourcePaths ? 'has-source' : ''}">
              <div class="volumes-cell-name min-w-0">
                <span class="volumes-mobile-label volumes-field-label">Storage Name</span>
                <div class="flex min-w-0 items-center gap-2">
                  <div class="min-w-0 flex-1">
                    <FormInput id="forms.{v.id}.name" required readonly={v.is_shared} bind:value={f.name} />
                  </div>
                </div>
              </div>
              {#if hasSourcePaths}
                <div class="volumes-cell-source min-w-0">
                  <span class="volumes-mobile-label volumes-field-label">Source Path</span>
                  {#if v.host_path}<FormInput id="source-{v.id}" aria-label="Source Path" value={v.host_path} readonly />{:else}<span class="data-table-cell-dash">-</span>{/if}
                </div>
              {/if}
              <div class="volumes-cell-dest min-w-0">
                <span class="volumes-mobile-label volumes-field-label">Destination Path</span>
                <FormInput id="forms.{v.id}.mountPath" required placeholder="/path/in/container" bind:value={f.mount_path} />
              </div>
              {#if data.supports_preview_suffix}
                <div class="volumes-col-pr min-w-0">
                  <div class="volumes-mobile-label volumes-field-label flex items-center gap-1.5"><span>PR suffix</span>{@render prHelper()}</div>
                  <Listbox
                    id="forms.{v.id}.isPreviewSuffixEnabled"
                    bind:value={f.is_preview_suffix_enabled}
                    onchange={() => saveVolume(v.id)}
                    options={[{ value: true, label: 'Add suffix' }, { value: false, label: 'Share volume' }]}
                  />
                </div>
              {/if}
              <div class="volumes-col-backup flex items-center justify-center gap-1.5">
                <span class="volumes-mobile-label volumes-field-label">Backup</span>
                {@render backupCell(v.id)}
              </div>
              <div class="volumes-col-actions volumes-cell-actions flex flex-nowrap items-center justify-end gap-1.5">
                <button type="submit" class="sr-only" tabindex="-1" aria-hidden="true">Update</button>
                {#if v.is_shared && !(canUpdate && (data.show_backup_action || data.database_backup_available))}
                  <button type="button" class="icon-button shrink-0" title="Update storage" aria-label="Update {f.name}" onclick={() => saveVolume(v.id)}>
                    <Reicon name="check" class="size-3.5" />
                  </button>
                {:else}
                  {#snippet storageActions()}
                    <RowAction onclick={() => saveVolume(v.id)}>Update</RowAction>
                    {#if canUpdate}
                      {#if data?.show_backup_action && !v.is_shared}
                        <RowAction onclick={() => openBackup(v.id)}>Backup</RowAction>
                      {:else if data?.database_backup_available}
                        <RowAction onclick={() => (databaseBackupOpen = true)}>Backup</RowAction>
                      {/if}
                    {/if}
                  {/snippet}
                  {#if v.is_shared}
                    <RowActions label="Storage actions" children={storageActions} />
                  {:else}
                    <RowActions label="Storage actions" children={storageActions}>
                      {#snippet danger()}
                        <RowAction onclick={() => (deleting = v)}>Delete</RowAction>
                      {/snippet}
                    </RowActions>
                  {/if}
                {/if}
              </div>
            </div>
          </form>
        {/if}
      {/each}
    </div>
  </div>
{/snippet}

{#snippet fileList(list: any[])}
  <div class="flex flex-col gap-4 p-4">
    {#each list as fs (fs.id)}
      <FileStorage {kind} {uuid} {fs} {canUpdate} isApplication={data.is_application} gitBased={data.git_based} preserveRepository={data.preserve_repository} onchange={load} />
    {/each}
  </div>
{/snippet}

{#if data}
  <div class="flex flex-col gap-6">
    {#if volumesOnly}
      {@render volumeTable()}
    {:else if !data.is_stack}
      <SettingsSection
        id="storage-mounts-section"
        title="Persistent storage"
        flush
        description={data.is_application && data.git_based ? 'Preview deployment volumes can use a -pr-#PRNumber suffix so each pull request receives isolated storage.' : 'Mount volumes, files, or directories to preserve data between deployments.'}
      >
        {#snippet actions()}
          {#if data.build_pack !== 'dockercompose' && canUpdate}
            <Dropdown panelClass="z-[90]! w-52! min-w-52!" role="menu">
              {#snippet trigger({ open: isOpen })}
                <button type="button" class="button button-highlighted" aria-haspopup="menu" aria-expanded={isOpen}>
                  <Reicon name="plus" class="size-3.5" />
                  Add mount
                  <Reicon name="chevron-down" class="size-3 opacity-55" />
                </button>
              {/snippet}
              {#snippet children({ close })}
                <button type="button" class="listbox-option justify-start! gap-2.5!" role="menuitem" onclick={() => (close(), open('volume'))}><Reicon name="storages" class="size-3.5 shrink-0 opacity-70" />Volume mount</button>
                <button type="button" class="listbox-option justify-start! gap-2.5!" role="menuitem" onclick={() => (close(), open('file'))}><Reicon name="file" class="size-3.5 shrink-0 opacity-70" />File mount</button>
                <button type="button" class="listbox-option justify-start! gap-2.5!" role="menuitem" onclick={() => (close(), open('host-file'))}><Reicon name="file-content" class="size-3.5 shrink-0 opacity-70" />Host file mount</button>
                <button type="button" class="listbox-option justify-start! gap-2.5!" role="menuitem" onclick={() => (close(), open('directory'))}><Reicon name="folder" class="size-3.5 shrink-0 opacity-70" />Directory mount</button>
              {/snippet}
            </Dropdown>
          {/if}
          {@render tabs()}
        {/snippet}
        {#if !hasAny}
          <Empty size="sm" title="No persistent storage" description="Add a volume, file, or directory mount to preserve data between deployments." iconName="storages" />
        {:else if activeTab === 'volumes'}
          {#if volumes.length > 0}
            {@render volumeTable()}
          {:else}
            <Empty size="sm" title="No volumes configured" description="Switch tabs or add a volume mount." iconName="storages" />
          {/if}
        {:else if activeTab === 'files'}
          {#if files.length > 0}
            {@render fileList(files)}
          {:else}
            <div class="flex flex-col gap-4 p-4"><Empty size="sm" title="No file mounts configured" description="Switch tabs or add a file mount." iconName="file" /></div>
          {/if}
        {:else if directories.length > 0}
          {@render fileList(directories)}
        {:else}
          <div class="flex flex-col gap-4 p-4"><Empty size="sm" title="No directory mounts configured" description="Switch tabs or add a directory mount." iconName="folder" /></div>
        {/if}
      </SettingsSection>
    {:else}
      <SettingsSection id="storage-service-{data.uuid}" title={data.name.replace(/[-_]+/g, ' ').replace(/\b\w/g, (c: string) => c.toUpperCase())} flush description="Volume mounts for this compose service. Compose-managed mounts are read-only in the dashboard.">
        {#snippet actions()}
          {@render tabs()}
        {/snippet}
        {#if !hasAny}
          <Empty size="sm" title="No storage found" description="No volumes, files, or directories are defined for this service." iconName="storages" />
        {:else if activeTab === 'volumes'}
          {#if volumes.length > 0}
            {@render volumeTable()}
          {:else}
            <Empty size="sm" title="No volumes configured" description="This service has no volume mounts." iconName="storages" />
          {/if}
        {:else if activeTab === 'files'}
          {#if files.length > 0}
            {@render fileList(files)}
          {:else}
            <div class="flex flex-col gap-4 p-4"><Empty size="sm" title="No file mounts configured" description="This service has no file mounts." iconName="file" /></div>
          {/if}
        {:else if directories.length > 0}
          {@render fileList(directories)}
        {:else}
          <div class="flex flex-col gap-4 p-4"><Empty size="sm" title="No directory mounts configured" description="This service has no directory mounts." iconName="folder" /></div>
        {/if}
      </SettingsSection>
    {/if}
  </div>

  {#if modal === 'volume'}
    <Modal narrow title="Add volume mount" onclose={() => (modal = null)}>
      {#if data.volume_unavailable_reason}
        <Callout type="warning" title="Volume mounts are unavailable" class="w-full">{data.volume_unavailable_reason}</Callout>
      {:else}
        <form class="flex w-full flex-col gap-4" onsubmit={(e) => submitNew(e, 'volume', { name, mount_path: mountPath }, 'volumes')}>
          <p class="text-[13px] leading-5 text-neutral-500 dark:text-fg-dim">
            Mount a Docker volume inside the container. coolify-rs adds the prefix <code class="break-all">{data?.uuid ?? uuid}-</code> to the name, so that the volume name is unique on the server.
          </p>
          <div class="flex flex-col gap-4">
            <FormInput placeholder="pv-name" id="name" label="Name" required helper="Volume name." bind:value={name} error={error?.field('name')} />
            <FormInput placeholder="/tmp/root" id="mount_path" label="Destination Path" required helper="Directory inside the container." bind:value={mountPath} error={error?.field('mount_path')} />
            <div class="flex justify-end pt-2"><button type="submit" class="button">Add volume</button></div>
          </div>
        </form>
      {/if}
    </Modal>
  {:else if modal === 'file'}
    <Modal narrow title="Add file mount" onclose={() => (modal = null)}>
      <form class="flex w-full flex-col gap-4" onsubmit={(e) => submitNew(e, 'file', { file_storage_path: filePath, file_storage_source: fileSource, file_storage_content: fileContent === '' ? null : fileContent }, 'files')}>
        <p class="text-[13px] leading-5 text-neutral-500 dark:text-fg-dim">Create a managed file on the host and mount it inside the container.</p>
        <div class="flex flex-col gap-4">
          <div class="rounded-lg bg-neutral-100 p-3 text-xs ring-1 ring-neutral-200 dark:bg-white/[0.04] dark:ring-white/[0.07]">
            <div class="mb-1 font-medium">Host file path</div>
            <code class="break-all">{previewPath}</code>
          </div>
          <FormInput placeholder="/etc/nginx/nginx.conf" id="file_storage_path" label="Destination Path" required helper="File location inside the container" bind:value={filePath} error={error?.field('file_storage_path')} />
          <FormInput placeholder="/srv/config/app.conf" id="file_storage_source" label="Source Path" helper="Optional. File location on the host: an absolute path anywhere on the host, or a path relative to the resource directory. Empty: coolify-rs keeps the file in the resource directory. coolify-rs never deletes a file outside the resource directory." bind:value={fileSource} />
          <FormTextarea label="Content" id="file_storage_content" bind:value={fileContent} />
          <div class="flex justify-end pt-2"><button type="submit" class="button">Add file</button></div>
        </div>
      </form>
    </Modal>
  {:else if modal === 'host-file'}
    <Modal narrow title="Add host file mount" onclose={() => (modal = null)}>
      <form class="flex w-full flex-col gap-4" onsubmit={(e) => submitNew(e, 'host-file', { host_file_storage_source: hostSource, host_file_storage_destination: hostDestination }, 'files')}>
        <p class="text-[13px] leading-5 text-neutral-500 dark:text-fg-dim">Bind an existing host file into the container. coolify-rs will not modify or delete the source file.</p>
        <div class="flex flex-col gap-4">
          <FormInput placeholder="/etc/nginx/nginx.conf" id="host_file_storage_source" label="Host File Path" required helper="Existing file on the host system." bind:value={hostSource} error={error?.field('host_file_storage_source')} />
          <FormInput placeholder="/etc/nginx/nginx.conf" id="host_file_storage_destination" label="Destination Path" required helper="File location inside the container." bind:value={hostDestination} error={error?.field('host_file_storage_destination')} />
          <div class="flex justify-end pt-2"><button type="submit" class="button">Add host file</button></div>
        </div>
      </form>
    </Modal>
  {:else if modal === 'directory'}
    <Modal narrow title="Add directory mount" onclose={() => (modal = null)}>
      <form class="flex w-full flex-col gap-4" onsubmit={(e) => submitNew(e, 'directory', { file_storage_directory_source: dirSource, file_storage_directory_destination: dirDestination }, 'directories')}>
        <p class="text-[13px] leading-5 text-neutral-500 dark:text-fg-dim">Bind a directory from the host system into the container.</p>
        <div class="flex flex-col gap-4">
          <FormInput placeholder="{hostPath}/etc/nginx" id="file_storage_directory_source" label="Source Directory" required helper="Directory on the host: an absolute path anywhere on the host, or a path relative to the resource directory. coolify-rs creates it when it is missing, and never deletes a directory outside the resource directory." bind:value={dirSource} error={error?.field('file_storage_directory_source')} />
          <FormInput placeholder="/etc/nginx" id="file_storage_directory_destination" label="Destination Directory" required helper="Directory inside the container." bind:value={dirDestination} error={error?.field('file_storage_directory_destination')} />
          <div class="flex justify-end pt-2"><button type="submit" class="button">Add directory</button></div>
        </div>
      </form>
    </Modal>
  {/if}

  {#if databaseBackupOpen}
    <NewScheduledBackup {uuid} onclose={() => (databaseBackupOpen = false)} />
  {/if}
  {#if backupModal !== null}
    <Modal title="Configure Volume Backup" onclose={() => (backupModal = null)}>
      <BackupTargetForm
        kind={backupOwner}
        uuid={backupOwnerUuid}
        targets={backupTargets}
        selectedTargetKey="volume:{backupModal}"
        oncreated={(b) => {
          backupModal = null;
          load();
          router.go(`${data?.backup_base}/${b}`);
        }}
      />
    </Modal>
  {/if}

  {#if pendingRename}
    {@const rename = pendingRename}
    <Modal title="Change the volume name?" focusFirstField={false} onclose={cancelRename}>
      <Callout type="warning" title="The data does not move to the new volume" class="mb-4">
        <ul class="mt-2 ml-4 list-disc">
          <li>Changing the name does not rename or move the Docker volume.</li>
          <li>After the next restart or deployment, the resource uses the Docker volume <strong>{rename.to}</strong>. If that volume does not exist, Docker creates it empty.</li>
          <li>Your data stays in <strong>{rename.from}</strong>. coolify-rs does not copy it, and does not delete it when you delete this resource.</li>
          <li>To keep your data, stop the resource and copy the data into the new volume before you start it again.</li>
        </ul>
      </Callout>

      <div class="mt-4 flex flex-wrap justify-end gap-2 border-t border-neutral-200 pt-4 dark:border-white/[0.08]">
        <button type="button" class="button" onclick={cancelRename}>Cancel</button>
        <button type="button" class="button button-error" onclick={() => saveVolume(rename.storageId, undefined, true)}>Change Volume Name</button>
      </div>
    </Modal>
  {/if}

  {#if deleting}
    <ConfirmModal
      title="Confirm persistent storage deletion?"
      actions={['The selected persistent storage/volume will be permanently deleted.', 'If the persistent storage/volume is actvily used by a resource data will be lost.']}
      confirmationText={deleting.name}
      confirmationLabel="Please confirm the execution of the actions by entering the Storage Name below"
      shortConfirmationLabel="Storage Name"
      confirmWithPassword
      onconfirm={async (password) => {
        const v = deleting;
        if (!v) return;
        try {
          await request('POST', `${base}/volume/${v.id}/delete`, { password, selected: [] });
        } catch (err) {
          const f = err instanceof ApiError ? err : new ApiError(0, String(err));
          if (f.status === 422) return f.message;
          notify.failure(f.message);
        }
        deleting = null;
        await load();
        onchange?.();
      }}
      onclose={() => (deleting = null)}
    />
  {/if}
{/if}
