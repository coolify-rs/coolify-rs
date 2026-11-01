<script lang="ts">
  // Port of upstream livewire/project/service/file-storage.blade.php (FileStorage)
  // for a mount of a compose application or database.
  import { untrack } from 'svelte';
  import Callout from './Callout.svelte';
  import ConfirmModal from './ConfirmModal.svelte';
  import FormInput from './FormInput.svelte';
  import FormTextarea from './FormTextarea.svelte';
  import MonacoEditor from './MonacoEditor.svelte';
  import UnsavedBar from './UnsavedBar.svelte';
  import { ApiError } from '../lib/api';
  import { servicesApi, type ServiceFileMount } from '../lib/servicesApi';
  import { notify } from '../lib/toast.svelte';

  let {
    serviceUuid,
    file,
    canUpdate,
    onchanged,
  }: { serviceUuid: string; file: ServiceFileMount; canUpdate: boolean; onchanged: () => Promise<void> } = $props();

  let content = $state(untrack(() => file.content ?? ''));
  let savedContent = $state(untrack(() => file.content ?? ''));
  let saving = $state(false);
  let confirm = $state<'delete' | 'to-file' | 'to-directory' | null>(null);

  $effect(() => {
    const next = file.content ?? '';
    untrack(() => {
      if (content === savedContent) content = next;
      savedContent = next;
    });
  });

  const dirty = $derived(content !== savedContent);
  const isYaml = $derived(/\.(yml|yaml)$/i.test(file.fs_path));
  const kind = $derived(file.is_directory ? 'directory' : 'file');
  const deletionActions = $derived([
    file.is_host_file ? 'The mount will be removed from the container.' : `The selected ${kind} will be permanently deleted from the container.`,
    ...(file.can_delete_from_server
      ? []
      : [`Only the mount configuration will be removed. The ${kind} at ${file.fs_path} stays on the server.`]),
  ]);
  const deletionCheckboxes = $derived(
    file.can_delete_from_server
      ? [
          {
            id: 'permanently_delete',
            label: file.is_directory
              ? 'The selected directory and all its contents will be permanently deleted from the server.'
              : 'The selected file will be permanently deleted from the server.',
            checked: true,
          },
        ]
      : [],
  );
  const contentLabel = $derived(file.is_based_on_git ? 'Content (refreshed after a successful deployment)' : 'Content');
  const contentHelper = "The content shown may be outdated. Click 'Load from server' to fetch the latest version.";
  const readonlyContent = $derived(file.is_based_on_git || file.is_binary || file.is_too_large);

  const message = (e: unknown) => (e instanceof ApiError ? e.message : String(e));

  async function submit() {
    saving = true;
    try {
      notify.success((await servicesApi.saveFile(serviceUuid, file.id, content)).message);
      savedContent = content;
      await onchanged();
    } catch (e) {
      notify.error(message(e));
    } finally {
      saving = false;
    }
  }

  async function load() {
    try {
      notify.success((await servicesApi.loadFile(serviceUuid, file.id)).message);
    } catch (e) {
      notify.error(message(e));
    }
    await onchanged();
  }

  async function convert(to: 'directory' | 'file') {
    try {
      await servicesApi.convertFile(serviceUuid, file.id, to);
    } catch (e) {
      notify.error(message(e));
    }
    await onchanged();
  }

  async function remove(password: string, selected: string[]) {
    try {
      notify.success((await servicesApi.deleteFile(serviceUuid, file.id, password, selected)).message);
      await onchanged();
    } catch (e) {
      if (e instanceof ApiError && e.status === 422 && e.message.includes('password')) return e.message;
      notify.error(message(e));
    }
  }
</script>

<div>
  <div class="flex flex-col gap-4">
    {#if file.is_too_large}
      <Callout type="warning" title="File too large">File on server exceeds 5 MB and cannot be edited from the UI. Edit it directly on the server.</Callout>
    {:else if file.is_host_file}
      <Callout type="info" title="Host-managed file">This host file mount is bind-only. coolify-rs will not create, edit, load, chmod, or delete the source file.</Callout>
    {:else if file.is_read_only}
      <Callout type="info" title="Read-only mount">
        {#if file.is_directory}
          This directory is mounted as read-only. You can convert it to a file, but you cannot delete it from the UI.
        {:else}
          This file is mounted as read-only. You cannot edit or delete it from the UI.
        {/if}
      </Callout>
    {/if}
    <div class="flex flex-col justify-center text-sm select-text">
      <div class="grid gap-4 md:grid-cols-2">
        <FormInput id="fs-{file.id}" label="Source Path" value={file.fs_path} readonly />
        <FormInput id="mount-{file.id}" label="Destination Path" value={file.mount_path} readonly />
      </div>
    </div>
    <form
      class="flex flex-col gap-4"
      onsubmit={(e) => {
        e.preventDefault();
        submit();
      }}
    >
      <UnsavedBar {dirty} {saving} onsave={submit} onreset={() => (content = savedContent)} />
      {#if canUpdate}
        <div class="flex flex-wrap items-center gap-2">
          {#if file.is_host_file}
            {#if !file.is_read_only}
              <button type="button" class="button button-error" onclick={() => (confirm = 'delete')}>Delete</button>
            {/if}
          {:else if file.is_directory}
            <button type="button" class="button" onclick={() => (confirm = 'to-file')}>Convert to file</button>
            {#if !file.is_read_only}
              <button type="button" class="button button-error" onclick={() => (confirm = 'delete')}>Delete</button>
            {/if}
          {:else}
            {#if !file.is_binary && !file.is_too_large}
              <button type="button" class="button" onclick={() => (confirm = 'to-directory')}>Convert to directory</button>
            {/if}
            <button type="button" class="button" onclick={load}>Load from server</button>
            {#if !file.is_read_only}
              <button type="button" class="button button-error" onclick={() => (confirm = 'delete')}>Delete</button>
            {/if}
          {/if}
        </div>
      {/if}
      {#if !file.is_directory && !file.is_host_file}
        {#if canUpdate}
          {#if isYaml}
            <div>
              <span class="mb-1.5 block text-[12px] font-medium">{contentLabel}</span>
              <MonacoEditor bind:value={content} language="yaml" readonly={file.is_read_only || readonlyContent} rows={20} />
            </div>
          {:else}
            <FormTextarea
              id="content-{file.id}"
              label={contentLabel}
              helper={contentHelper}
              rows={20}
              bind:value={content}
              disabled={file.is_read_only}
              readonly={readonlyContent}
            />
          {/if}
        {:else}
          <Callout type="info" title="Hidden (only admins can view)">The file content can contain secrets.</Callout>
        {/if}
      {/if}
    </form>
  </div>
</div>

{#if confirm === 'delete'}
  <ConfirmModal
    title={file.is_host_file ? 'Confirm Host File Mount Removal?' : file.is_directory ? 'Confirm Directory Deletion?' : 'Confirm File Deletion?'}
    checkboxes={deletionCheckboxes}
    actions={deletionActions}
    confirmationText={file.confirm_path}
    confirmationLabel="Please confirm the execution of the actions by entering the Filepath below"
    shortConfirmationLabel="Filepath"
    confirmWithPassword
    onconfirm={remove}
    onclose={() => (confirm = null)}
  />
{:else if confirm === 'to-file'}
  <ConfirmModal
    title="Confirm Directory Conversion to File?"
    actions={['All files in this directory will be permanently deleted and an empty file will be created in its place.']}
    confirmationText={file.confirm_path}
    confirmationLabel="Please confirm the execution of the actions by entering the Filepath below"
    shortConfirmationLabel="Filepath"
    confirmWithPassword={false}
    step2ButtonText="Convert to file"
    onconfirm={async () => {
      convert('file');
    }}
    onclose={() => (confirm = null)}
  />
{:else if confirm === 'to-directory'}
  <ConfirmModal
    title="Confirm File Conversion to Directory?"
    actions={['The selected file will be permanently deleted and an empty directory will be created in its place.']}
    confirmationText={file.confirm_path}
    confirmationLabel="Please confirm the execution of the actions by entering the Filepath below"
    shortConfirmationLabel="Filepath"
    confirmWithPassword={false}
    step2ButtonText="Convert to directory"
    onconfirm={async () => {
      convert('directory');
    }}
    onclose={() => (confirm = null)}
  />
{/if}
