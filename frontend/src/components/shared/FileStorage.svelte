<script lang="ts">
  // Port of upstream livewire/project/service/file-storage.blade.php (Project\Service\FileStorage).
  import Callout from '../Callout.svelte';
  import Checkbox from '../Checkbox.svelte';
  import ConfirmModal from '../ConfirmModal.svelte';
  import FormInput from '../FormInput.svelte';
  import FormTextarea from '../FormTextarea.svelte';
  import Listbox from '../Listbox.svelte';
  import UnsavedBar from '../UnsavedBar.svelte';
  import { ApiError, request } from '../../lib/api';
  import { resPath, type ResourceKind } from '../../lib/sharedApi';
  import { notify } from '../../lib/toast.svelte';

  let {
    kind,
    uuid,
    fs,
    canUpdate,
    isApplication,
    gitBased,
    preserveRepository,
    onchange,
  }: {
    kind: ResourceKind;
    uuid: string;
    fs: any;
    canUpdate: boolean;
    isApplication: boolean;
    gitBased: boolean;
    preserveRepository: boolean;
    onchange: () => void;
  } = $props();

  const base = $derived(`${resPath(kind, uuid)}/storages/file/${fs.id}`);
  let content = $state('');
  let saved = $state('');
  let isBasedOnGit = $state(false);
  let suffix = $state(true);
  let confirm = $state<'delete' | 'to-file' | 'to-directory' | null>(null);
  let saving = $state(false);

  $effect(() => {
    content = saved = fs.content ?? '';
    isBasedOnGit = fs.is_based_on_git;
    suffix = fs.is_preview_suffix_enabled;
  });

  const label = $derived(fs.is_based_on_git ? 'Content (refreshed after a successful deployment)' : 'Content');
  const contentHelp = "The content shown may be outdated. Click 'Load from server' to fetch the latest version.";
  const fsKind = $derived(fs.is_directory ? 'directory' : 'file');
  const deletionActions = $derived([
    fs.is_host_file ? 'The mount will be removed from the container.' : `The selected ${fsKind} will be permanently deleted from the container.`,
    ...(fs.can_delete_from_server ? [] : [`Only the mount configuration will be removed. The ${fsKind} at ${fs.fs_path} is not deleted on the server.`]),
  ]);
  const deletionCheckboxes = $derived(
    fs.can_delete_from_server
      ? [{ id: 'permanently_delete', label: fs.is_directory ? 'The selected directory and all its contents will be permanently deleted from the server.' : 'The selected file will be permanently deleted from the server.' }]
      : [],
  );
  const deleteTitle = $derived(fs.is_host_file ? 'Confirm Host File Mount Removal?' : fs.is_directory ? 'Confirm Directory Deletion?' : 'Confirm File Deletion?');

  function failure(err: unknown) {
    notify.failure(err instanceof ApiError ? err.message : String(err));
  }

  const payload = () => ({ content: content === '' && fs.content === null ? null : content, is_based_on_git: isBasedOnGit, is_preview_suffix_enabled: suffix });

  async function submit() {
    if (saving) return;
    saving = true;
    try {
      const r = await request<{ message: string }>('PATCH', base, payload());
      notify.success(r.message);
      onchange();
    } catch (err) {
      failure(err);
    } finally {
      saving = false;
    }
  }

  async function instantSave() {
    try {
      const r = await request<{ message: string }>('POST', `${base}/instant`, payload());
      notify.success(r.message);
      onchange();
    } catch (err) {
      failure(err);
      onchange();
    }
  }

  async function loadFromServer() {
    try {
      const r = await request<{ message: string }>('POST', `${base}/load`, {});
      notify.success(r.message);
    } catch (err) {
      failure(err);
    }
    onchange();
  }

  async function convert(to: 'file' | 'directory') {
    try {
      await request('POST', `${base}/convert`, { to });
    } catch (err) {
      failure(err);
    }
    confirm = null;
    onchange();
  }
</script>

<div>
  <div class="flex flex-col gap-4">
    {#if fs.is_too_large}
      <Callout type="warning" title="File too large">File on server exceeds 5 MB and cannot be edited from the UI. Edit it directly on the server.</Callout>
    {:else if fs.is_host_file}
      <Callout type="info" title="Host-managed file">This host file mount is bind-only. Coolify will not create, edit, load, chmod, or delete the source file.</Callout>
    {:else if fs.is_read_only}
      <Callout type="info" title="Read-only mount">
        {#if fs.is_directory}
          This directory is mounted as read-only. You can convert it to a file, but you cannot delete it from the UI.
        {:else}
          This file is mounted as read-only. You cannot edit or delete it from the UI.
        {/if}
      </Callout>
    {/if}
    <div class="flex flex-col justify-center text-sm select-text">
      <div class="grid gap-4 md:grid-cols-2">
        <FormInput id="fs-{fs.id}-source" label="Source Path" value={fs.fs_path} readonly />
        <FormInput id="fs-{fs.id}-destination" label="Destination Path" value={fs.mount_path} readonly />
      </div>
    </div>
    {#if isApplication && gitBased && canUpdate}
      <div class="w-full sm:w-96">
        <Listbox
          id="fs-{fs.id}-suffix"
          label="PR deployment suffix"
          helper="Choose whether preview deployments receive an isolated -pr-N path suffix."
          bind:value={suffix}
          onchange={instantSave}
          options={[{ value: true, label: 'Add suffix' }, { value: false, label: 'Share path' }]}
        />
      </div>
    {/if}
    <form
      onsubmit={(e) => {
        e.preventDefault();
        submit();
      }}
      class="flex flex-col gap-4"
    >
      <UnsavedBar dirty={content !== saved} {saving} onsave={submit} onreset={() => (content = saved)} />
      {#if canUpdate}
        <div class="flex flex-wrap items-center gap-2">
          {#if fs.is_host_file}
            {#if !fs.is_read_only}
              <button type="button" class="button button-error" onclick={() => (confirm = 'delete')}>Delete</button>
            {/if}
          {:else if fs.is_directory}
            <button type="button" class="button" onclick={() => (confirm = 'to-file')}>Convert to file</button>
            {#if !fs.is_read_only}
              <button type="button" class="button button-error" onclick={() => (confirm = 'delete')}>Delete</button>
            {/if}
          {:else}
            {#if !fs.is_binary && !fs.is_too_large}
              <button type="button" class="button" onclick={() => (confirm = 'to-directory')}>Convert to directory</button>
            {/if}
            <button type="button" class="button" onclick={loadFromServer}>Load from server</button>
            {#if !fs.is_read_only}
              <button type="button" class="button button-error" onclick={() => (confirm = 'delete')}>Delete</button>
            {/if}
          {/if}
        </div>
      {/if}
      {#if !fs.is_directory && !fs.is_host_file}
        {#if preserveRepository}
          <div class="w-full sm:w-96">
            <Checkbox id="fs-{fs.id}-git" label="Is this based on the Git repository?" disabled={!canUpdate || fs.is_read_only} bind:checked={isBasedOnGit} onchange={instantSave} />
          </div>
        {/if}
        {#if canUpdate}
          <FormTextarea
            {label}
            helper={contentHelp}
            rows={20}
            id="fs-{fs.id}-content"
            bind:value={content}
            disabled={fs.is_read_only}
            readonly={!fs.is_read_only && (fs.is_based_on_git || fs.is_binary || fs.is_too_large)}
          />
        {:else}
          <Callout type="info" title="Hidden (only admins can view)">The file content can contain secrets.</Callout>
        {/if}
      {/if}
    </form>
  </div>
</div>

{#if confirm === 'delete'}
  <ConfirmModal
    title={deleteTitle}
    checkboxes={deletionCheckboxes}
    actions={deletionActions}
    confirmationText={fs.shown_path}
    confirmationLabel="Please confirm the execution of the actions by entering the Filepath below"
    shortConfirmationLabel="Filepath"
    confirmWithPassword
    onconfirm={async (password, selected) => {
      try {
        const r = await request<{ message: string }>('POST', `${base}/delete`, { password, selected });
        notify.success(r.message);
      } catch (err) {
        const f = err instanceof ApiError ? err : new ApiError(0, String(err));
        if (f.status === 422) return f.message;
        notify.failure(f.message);
      }
      confirm = null;
      onchange();
    }}
    onclose={() => (confirm = null)}
  />
{:else if confirm === 'to-file'}
  <ConfirmModal
    title="Confirm Directory Conversion to File?"
    actions={['All files in this directory will be permanently deleted and an empty file will be created in its place.']}
    confirmationText={fs.shown_path}
    confirmationLabel="Please confirm the execution of the actions by entering the Filepath below"
    shortConfirmationLabel="Filepath"
    step2ButtonText="Convert to file"
    onconfirm={() => convert('file')}
    onclose={() => (confirm = null)}
  />
{:else if confirm === 'to-directory'}
  <ConfirmModal
    title="Confirm File Conversion to Directory?"
    actions={['The selected file will be permanently deleted and an empty directory will be created in its place.']}
    confirmationText={fs.shown_path}
    confirmationLabel="Please confirm the execution of the actions by entering the Filepath below"
    shortConfirmationLabel="Filepath"
    step2ButtonText="Convert to directory"
    onconfirm={() => convert('directory')}
    onclose={() => (confirm = null)}
  />
{/if}
