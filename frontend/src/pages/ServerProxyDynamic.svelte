<script lang="ts">
  // Port of upstream livewire/server/proxy/dynamic-configurations.blade.php,
  // server/proxy/new-dynamic-configuration.blade.php and server/proxy/dynamic-configuration-navbar.blade.php.
  import Reicon from '../components/Reicon.svelte';
  import Empty from '../components/Empty.svelte';
  import FormInput from '../components/FormInput.svelte';
  import FormTextarea from '../components/FormTextarea.svelte';
  import Modal from '../components/Modal.svelte';
  import MonacoEditor from '../components/MonacoEditor.svelte';
  import ServerFrame from '../components/ServerFrame.svelte';
  import SettingsSection from '../components/SettingsSection.svelte';
  import StatusBadge from '../components/StatusBadge.svelte';
  import { ApiError } from '../lib/api';
  import type { ServerFacts } from '../lib/api';
  import { get, post, failure } from '../lib/serverApi';
  import { session } from '../lib/session.svelte';
  import { notify } from '../lib/toast.svelte';

  let { uuid }: { uuid: string } = $props();
  type File = { name: string; content: string | null; managed: boolean };
  let files = $state<File[]>([]);
  let loaded = $state(false);
  let facts = $state<ServerFacts | null>(null);
  let editing = $state<{ fileName: string; value: string; newFile: boolean; title: string } | null>(null);
  let error = $state<ApiError | null>(null);
  const base = $derived(`/server-page/${encodeURIComponent(uuid)}/proxy/dynamic`);
  const canEdit = $derived(session.isAdmin);

  async function load(toast = true) {
    try {
      const r = await get<{ files: File[]; warning?: string | null }>(base);
      files = r.files;
      loaded = true;
      if (r.warning) notify.info(r.warning);
      if (toast && canEdit) notify.success('Dynamic configurations loaded.');
    } catch (e) {
      failure(e);
    }
  }
  async function save(e: Event) {
    e.preventDefault();
    if (!editing) return;
    error = null;
    try {
      await post(base, { file_name: editing.fileName, value: editing.value, new_file: editing.newFile });
      editing = null;
      notify.success('Dynamic configuration saved.');
      await load();
    } catch (err) {
      error = failure(err);
    }
  }
  async function remove(name: string) {
    try {
      await post(`${base}/delete`, { file_name: name });
      notify.success('File deleted.');
      await load();
    } catch (e) {
      failure(e);
    }
  }
</script>

<ServerFrame {uuid} active="proxy" sub="dynamic" title="Proxy Dynamic Configuration" onloaded={(f) => ((facts = f), f.functional && load())}>
  {#snippet children(f)}
    <div class="application-settings-form flex w-full flex-col gap-6">
      {#if f.functional}
        <div class="flex flex-wrap items-center justify-end gap-3 px-1">
          <div class="flex items-center gap-2">
            <button type="button" class="button" onclick={() => load()}>
              <Reicon name="refresh" class="size-3.5" />
              Reload
            </button>
            {#if canEdit}
              <button type="button" class="button" onclick={() => (editing = { fileName: '', value: '', newFile: true, title: 'New Dynamic Configuration' })}>+ Add</button>
            {/if}
          </div>
        </div>
        {#if files.length > 0}
          {#each files as file (file.name)}
            <SettingsSection title={file.name}>
              {#snippet actions()}
                {#if file.managed}
                  <StatusBadge label="Managed" type="neutral" />
                {:else if canEdit}
                  <div class="flex items-center gap-2">
                    <button type="button" class="button" onclick={() => (editing = { fileName: file.name, value: file.content ?? '', newFile: false, title: 'Edit Configuration' })}>Edit</button>
                    <button type="button" class="button button-error" onclick={() => remove(file.name)}>Delete</button>
                  </div>
                {/if}
              {/snippet}
              {#if canEdit}
                <FormTextarea id={file.name} disabled value={file.content ?? ''} rows={8} />
              {:else}
                <p class="text-xs text-neutral-500 dark:text-fg-dim">You do not have permission to view the contents of this configuration.</p>
              {/if}
            </SettingsSection>
          {/each}
        {:else if loaded}
          <SettingsSection title="Dynamic configurations">
            <Empty size="sm" title="No dynamic configurations" description="Add a configuration file to extend the proxy at runtime." iconName="file-content" />
          </SettingsSection>
        {/if}
      {:else}
        <SettingsSection title="Dynamic configurations" description="Manage additional runtime proxy configuration.">
          <Empty size="sm" title="Server validation required" description="Validate this server before loading proxy configuration." iconName="file-content" />
        </SettingsSection>
      {/if}
    </div>
  {/snippet}
</ServerFrame>

{#if editing}
  <Modal title={editing.title} onclose={() => (editing = null)} isLarge={editing.title !== 'New Dynamic Configuration'}>
    <form class="flex flex-col w-full gap-4" onsubmit={save}>
      <FormInput id="fileName" label="Filename" required bind:value={editing.fileName} error={error?.field('file_name')} />
      <div>
        <!-- svelte-ignore a11y_label_has_associated_control -->
        <label class="flex gap-1 items-center mb-1 text-sm font-medium text-black dark:text-white">Configuration <span class="text-error">*</span></label>
        <MonacoEditor bind:value={editing.value} language={facts?.proxy_type === 'TRAEFIK' ? 'yaml' : ''} rows={20} />
        {#if error?.field('value')}<p class="mt-1 text-xs text-red-500">{error.field('value')}</p>{/if}
      </div>
      <button type="submit" class="button">Save</button>
    </form>
  </Modal>
{/if}
