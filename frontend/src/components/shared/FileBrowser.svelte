<script lang="ts">
  // Port of upstream livewire/project/shared/file-browser.blade.php (+ Shared\FileBrowser). The
  // heading and the sidebar come from the resource page around this section.
  import BetaBadge from '../BetaBadge.svelte';
  import Empty from '../Empty.svelte';
  import FormInput from '../FormInput.svelte';
  import Listbox from '../Listbox.svelte';
  import Modal from '../Modal.svelte';
  import MonacoEditor from '../MonacoEditor.svelte';
  import Reicon from '../Reicon.svelte';
  import SettingsSection from '../SettingsSection.svelte';
  import { ApiError, csrf, request } from '../../lib/api';
  import { resPath, type ResourceKind } from '../../lib/sharedApi';
  import { notify } from '../../lib/toast.svelte';

  let { kind, uuid }: { kind: ResourceKind; uuid: string } = $props();

  interface Entry {
    name: string;
    type: 'dir' | 'file' | 'symlink';
    size: number;
    mtime: number;
    perms: string;
    owner: string;
    group: string;
  }
  interface Listing {
    current_path: string;
    use_helper: boolean;
    shell_unavailable: boolean;
    helper_error: string | null;
    entries: Entry[];
    message?: string;
  }

  let filesLoaded = $state(false);
  let loadFailed = $state(false);
  let containerRunning = $state(false);
  let shellUnavailable = $state(false);
  let helperError = $state<string | null>(null);
  let useHelper = $state(false);
  let container = $state('');
  let available = $state<string[]>([]);
  let currentPath = $state('');
  let entries = $state<Entry[]>([]);
  let busy = $state<string | null>(null);

  // The modals.
  type Dialog =
    | { type: 'file' | 'folder' }
    | { type: 'chmod' | 'rename' | 'delete'; entry: Entry }
    | null;
  let dialog = $state<Dialog>(null);
  let dialogValue = $state('');
  let newMenu = $state(false);
  let rowMenu = $state<string | null>(null);
  let dragging = $state(false);
  let uploadInput = $state<HTMLInputElement | undefined>();

  // The editor.
  let editorOpen = $state(false);
  let editingPath = $state<string | null>(null);
  let editorContent = $state('');
  let editorLanguage = $state('');

  const base = $derived(`${resPath(kind, uuid)}/files`);

  function failure(err: unknown) {
    notify.failure(err instanceof ApiError ? err.message : String(err));
  }

  function apply(r: Listing) {
    currentPath = r.current_path;
    useHelper = r.use_helper;
    shellUnavailable = r.shell_unavailable;
    helperError = r.helper_error;
    entries = r.entries;
    if (r.message) notify.success(r.message);
  }

  // `run()`: a label over the listing, after a short delay for fast requests.
  async function run(label: string, action: () => Promise<void>) {
    const timer = setTimeout(() => (busy = label), 150);
    try {
      await action();
    } catch (err) {
      failure(err);
    } finally {
      clearTimeout(timer);
      busy = null;
    }
  }

  async function loadFiles() {
    loadFailed = false;
    filesLoaded = false;
    try {
      const r = await request<Listing & { container_running: boolean; container?: string; available_containers?: string[] }>('POST', `${base}/load`, {});
      containerRunning = r.container_running;
      if (r.container_running) {
        container = r.container ?? '';
        available = r.available_containers ?? [];
        apply(r);
      }
      filesLoaded = true;
    } catch (err) {
      loadFailed = true;
      failure(err);
    }
  }

  $effect(() => {
    loadFiles();
  });

  const state_ = () => ({ container, path: currentPath, use_helper: useHelper });

  const list = (body: Record<string, unknown>) => request<Listing>('POST', `${base}/list`, { ...state_(), ...body }).then(apply);
  const action = (op: string, body: Record<string, unknown>) => request<Listing>('POST', `${base}/action`, { ...state_(), op, ...body }).then(apply);

  function isHelperSymlink(entry: Entry): boolean {
    if (!useHelper || entry.type !== 'symlink') return false;
    notify.error('Symlinks cannot be opened in a container without a shell. Open the target path instead.');
    return true;
  }

  function open(entry: Entry) {
    if (isHelperSymlink(entry)) return;
    run('Loading files...', () => list({ op: 'open', name: entry.name }));
  }

  async function openEditor(entry: Entry) {
    rowMenu = null;
    if (isHelperSymlink(entry)) return;
    await run('Opening file...', async () => {
      const r = await request<{ content: string; language: string; editing_path: string }>('POST', `${base}/action`, { ...state_(), op: 'edit', name: entry.name });
      editorContent = r.content;
      editorLanguage = r.language;
      editingPath = r.editing_path;
      editorOpen = true;
    });
  }

  function saveEditor(e: Event) {
    e.preventDefault();
    const path = editingPath;
    const content = editorContent;
    editorOpen = false;
    run('Saving file...', () => action('save', { editing_path: path, content }));
  }

  function download(entry: Entry) {
    rowMenu = null;
    if (isHelperSymlink(entry)) return;
    run(entry.type === 'dir' ? 'Preparing folder download...' : 'Preparing file download...', async () => {
      const r = await request<{ url: string }>('POST', `${base}/download`, { ...state_(), name: entry.name });
      window.location.href = r.url;
    });
  }

  function uploadFile(file: File) {
    run('Uploading file...', async () => {
      const form = new FormData();
      form.append('container', container);
      form.append('path', currentPath);
      form.append('use_helper', String(useHelper));
      form.append('file', file);
      const res = await fetch(`${base}/upload`, { method: 'POST', credentials: 'same-origin', headers: { Accept: 'application/json', 'X-CSRF-Token': csrf() }, body: form });
      const d = await res.json().catch(() => ({}));
      if (!res.ok) throw new ApiError(res.status, d.message ?? 'Upload failed.');
      apply(d);
    });
  }

  function submitDialog(e: Event) {
    e.preventDefault();
    const d = dialog;
    const value = dialogValue;
    dialog = null;
    if (!d) return;
    if (d.type === 'file') run('Creating file...', () => action('create_file', { name: value }));
    else if (d.type === 'folder') run('Creating folder...', () => action('create_directory', { name: value }));
    else if (d.type === 'chmod') run('Updating permissions...', () => action('chmod', { name: d.entry.name, mode: value }));
    else if (d.type === 'rename') run('Renaming...', () => action('rename', { name: d.entry.name, new_name: value }));
  }

  const breadcrumbs = $derived.by(() => {
    const crumbs = [{ label: '/', path: '/' }];
    let accumulated = '';
    for (const segment of currentPath.split('/').filter(Boolean)) {
      accumulated += `/${segment}`;
      crumbs.push({ label: segment, path: accumulated });
    }
    return crumbs;
  });

  // `formatBytes()`.
  function formatBytes(bytes: number): string {
    if (!bytes || bytes < 0) return '0 B';
    const units = ['B', 'KB', 'MB', 'GB', 'TB', 'PB'];
    const exponent = Math.min(Math.floor(Math.log(bytes) / Math.log(1024)), units.length - 1);
    const value = bytes / Math.pow(1024, exponent);
    return `${Math.round(value * 100) / 100} ${units[exponent]}`;
  }

  // `Carbon::createFromTimestamp($mtime)->toDateTimeString()`, in UTC.
  const modified = (mtime: number) => (mtime ? new Date(mtime * 1000).toISOString().slice(0, 19).replace('T', ' ') : '-');

  function openDialog(next: Dialog, value = '') {
    newMenu = false;
    rowMenu = null;
    dialog = next;
    dialogValue = value;
  }
</script>

<svelte:window
  onkeydown={(e) => {
    if (e.key === 'Escape') {
      newMenu = false;
      rowMenu = null;
    }
  }}
/>

<SettingsSection title="Files" description="Browse and manage files inside the running container." flush>
  {#snippet badges()}
    <BetaBadge />
    {#if containerRunning && filesLoaded && available.length > 1}
      <div class="-my-0.5 w-full sm:w-72">
        <Listbox
          id="file-browser-container"
          searchable
          searchPlaceholder="Search containers"
          options={available.map((name) => ({ value: name, label: name }))}
          bind:value={container}
          onchange={() => run('Loading files...', () => list({ op: 'select' }))}
        />
      </div>
    {/if}
  {/snippet}

  {#snippet actions()}
    {#if containerRunning && filesLoaded && !shellUnavailable}
      <input
        type="file"
        class="hidden"
        bind:this={uploadInput}
        onchange={(e) => {
          const input = e.currentTarget;
          if (input.files?.length) {
            uploadFile(input.files[0]);
            input.value = '';
          }
        }}
      />
      <button type="button" class="button" title="Upload a file to this folder" onclick={() => uploadInput?.click()}>Upload</button>

      <div class="relative">
        <button type="button" class="button" aria-expanded={newMenu} onclick={() => (newMenu = !newMenu)}>
          New
          <Reicon name="chevron-down" class="size-3.5" />
        </button>
        {#if newMenu}
          <div class="listbox-panel top-full! right-0! left-auto! mt-1! min-w-36! w-36! z-50!">
            <button type="button" class="listbox-option" onclick={() => openDialog({ type: 'file' })}>
              <Reicon name="file" class="size-3.5 shrink-0" />
              <span class="flex-1 text-left">New file</span>
            </button>
            <button type="button" class="listbox-option" onclick={() => openDialog({ type: 'folder' })}>
              <Reicon name="folder" class="size-3.5 shrink-0" />
              <span class="flex-1 text-left">New folder</span>
            </button>
          </div>
        {/if}
      </div>

      <button type="button" class="button" title="Refresh this folder" onclick={() => run('Loading files...', () => list({ op: 'refresh' }))}>Refresh</button>
    {:else if !filesLoaded && !loadFailed}
      <button type="button" class="button" disabled>Upload</button>
      <button type="button" class="button" disabled>
        New
        <Reicon name="chevron-down" class="size-3.5" />
      </button>
      <button type="button" class="button" disabled>Refresh</button>
    {/if}
  {/snippet}

  {#if loadFailed}
    <Empty title="Could not load files" description="Check the server connection and try again." />
    <div class="flex justify-center pb-4">
      <button type="button" class="button" onclick={loadFiles}>Retry</button>
    </div>
  {:else if !filesLoaded}
    <div class="flex justify-center py-12">
      <div class="inline-flex items-center gap-2 text-[13px] text-neutral-500 dark:text-fg-dim">
        <span class="loading-indicator"></span>
        Loading files...
      </div>
    </div>
  {:else if !containerRunning}
    <Empty title="Container is not running" description="Start the container to browse its files." iconName="folder" />
  {:else if shellUnavailable}
    <Empty title="File browser is not available" description="Coolify cannot open the files of this container. {helperError ?? ''}" iconName="folder" />
  {:else}
    <div class="relative">
      {#if busy}
        <div class="absolute inset-0 z-10">
          <div class="flex h-full items-start justify-center">
            <div class="sticky top-[40vh] mt-12 inline-flex items-center gap-2 rounded-md bg-white px-3 py-1.5 text-[13px] text-neutral-500 shadow-sm ring-1 ring-black/5 dark:bg-coolgray-100 dark:text-fg-dim dark:ring-white/10">
              <span>{busy}</span>
            </div>
          </div>
        </div>
      {/if}
      <div class="transition-[filter,opacity] duration-150 motion-reduce:transition-none {busy ? 'pointer-events-none select-none blur-[2px] opacity-60' : ''}">
        <div class="flex flex-wrap items-center px-4 py-2.5 text-sm">
          {#each breadcrumbs as crumb, i (crumb.path)}
            {#if i > 1}
              <span class="text-coollabs-subtle">/</span>
            {/if}
            <button
              type="button"
              onclick={() => run('Loading files...', () => list({ op: 'go', to: crumb.path }))}
              class={i === breadcrumbs.length - 1 ? 'text-current font-medium cursor-default' : 'cursor-pointer text-coollabs-subtle underline-offset-4 hover:text-current hover:underline'}
            >
              {crumb.label}
            </button>
          {/each}
        </div>

        <!-- svelte-ignore a11y_no_static_element_interactions -->
        <div
          ondragover={(e) => {
            e.preventDefault();
            dragging = true;
          }}
          ondragleave={(e) => {
            e.preventDefault();
            dragging = false;
          }}
          ondrop={(e) => {
            e.preventDefault();
            dragging = false;
            if (e.dataTransfer?.files.length) uploadFile(e.dataTransfer.files[0]);
          }}
          class="transition-[box-shadow] duration-150 ease-out motion-reduce:transition-none {dragging ? 'ring-2 ring-coollabs/40 dark:ring-white/30' : ''}"
        >
          {#if entries.length === 0}
            <Empty size="sm" title="This folder is empty" description="Upload a file to get started." iconName="folder" />
          {:else}
            <div class="data-table w-full">
              <div class="data-table-header file-table-grid">
                <span></span>
                <span>Name</span>
                <span>Perms</span>
                <span>Owner</span>
                <span>Group</span>
                <span class="text-right">Size</span>
                <span>Modified</span>
                <span class="text-right">Actions</span>
              </div>
              {#each entries as entry (entry.name)}
                <div class="data-table-row file-table-grid">
                  <div class="flex items-center">
                    <Reicon name={entry.type === 'dir' ? 'folder-filled' : 'file'} class="size-4 {entry.type === 'dir' ? 'text-coollabs dark:text-neutral-200' : 'text-coollabs-subtle opacity-70'}" />
                  </div>
                  <div class="min-w-0">
                    {#if entry.type === 'dir'}
                      <button type="button" onclick={() => open(entry)} class="block w-full cursor-pointer truncate text-left font-medium hover:text-coollabs hover:underline dark:hover:text-white">{entry.name}</button>
                    {:else}
                      <span class="block truncate">{entry.name}</span>
                    {/if}
                  </div>
                  <div class="min-w-0">
                    <button
                      type="button"
                      title="Edit permissions"
                      class="cursor-pointer rounded-md px-1.5 py-0.5 font-mono text-xs text-coollabs-subtle hover:bg-black/5 hover:text-current dark:hover:bg-white/[0.06]"
                      onclick={() => openDialog({ type: 'chmod', entry }, entry.perms)}
                    >
                      {entry.perms || '-'}
                    </button>
                  </div>
                  <div class="min-w-0 truncate text-coollabs-subtle">{entry.owner || '-'}</div>
                  <div class="min-w-0 truncate text-coollabs-subtle">{entry.group || '-'}</div>
                  <div class="text-right text-coollabs-subtle tabular-nums">{entry.type === 'dir' ? '-' : formatBytes(entry.size)}</div>
                  <div class="text-coollabs-subtle tabular-nums">{modified(entry.mtime)}</div>
                  <div class="flex items-center justify-end">
                    <div class="relative">
                      <button
                        type="button"
                        title="Actions"
                        aria-label="Actions"
                        aria-expanded={rowMenu === entry.name}
                        onclick={() => (rowMenu = rowMenu === entry.name ? null : entry.name)}
                        class="inline-flex size-7 cursor-pointer items-center justify-center rounded-md text-coollabs-subtle hover:bg-black/5 hover:text-current focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-coollabs/40 dark:hover:bg-white/[0.06] dark:focus-visible:ring-white/30 {rowMenu === entry.name ? 'bg-black/5 text-current dark:bg-white/[0.08]' : ''}"
                      >
                        <Reicon name="dots-horizontal" class="size-4" />
                      </button>
                      {#if rowMenu === entry.name}
                        <div class="listbox-panel top-full! right-0! left-auto! mt-1! min-w-52! w-52! z-50!">
                          {#if entry.type !== 'dir'}
                            <button type="button" class="listbox-option" onclick={() => openEditor(entry)}>
                              <Reicon name="file-content" class="size-3.5 shrink-0" />
                              <span class="min-w-0 flex-1 text-left">Edit</span>
                            </button>
                          {/if}
                          <button type="button" class="listbox-option" onclick={() => download(entry)}>
                            <Reicon name="download" class="size-3.5 shrink-0" />
                            <span class="min-w-0 flex-1 text-left">Download</span>
                          </button>
                          <button type="button" class="listbox-option" onclick={() => openDialog({ type: 'rename', entry }, entry.name)}>
                            <Reicon name="edit" class="size-3.5 shrink-0" />
                            <span class="min-w-0 flex-1 text-left">Rename</span>
                          </button>
                          <button type="button" class="listbox-option" onclick={() => openDialog({ type: 'delete', entry })}>
                            <Reicon name="trash" class="size-3.5 shrink-0 text-red-600 dark:text-red-500" />
                            <span class="min-w-0 flex-1 text-left text-red-600 dark:text-red-500">Delete</span>
                          </button>
                        </div>
                      {/if}
                    </div>
                  </div>
                </div>
              {/each}
            </div>
          {/if}
        </div>
      </div>
    </div>
  {/if}
</SettingsSection>

{#if dialog?.type === 'delete'}
  {@const entry = dialog.entry}
  <Modal title="Delete this item?" onclose={() => (dialog = null)}>
    <div class="flex flex-col gap-4">
      <p class="text-sm text-coollabs-subtle">
        This permanently deletes
        <span class="font-medium">{entry.name}</span>
        inside the container. This cannot be undone.
      </p>
      <div class="flex justify-end">
        <button
          type="button"
          class="button button-error"
          onclick={() => {
            const { name, type } = entry;
            dialog = null;
            run(type === 'dir' ? 'Deleting folder...' : 'Deleting file...', () => action('delete', { name }));
          }}
        >
          Delete
        </button>
      </div>
    </div>
  </Modal>
{:else if dialog}
  {@const d = dialog}
  <Modal title={d.type === 'file' ? 'New file' : d.type === 'folder' ? 'New folder' : d.type === 'chmod' ? 'Change permissions' : 'Rename'} onclose={() => (dialog = null)}>
    <form onsubmit={submitDialog} class="flex flex-col gap-4">
      {#if d.type === 'chmod'}
        <p class="text-sm text-coollabs-subtle">
          Set the octal permission mode for
          <span class="font-medium">{d.entry.name}</span>.
        </p>
        <FormInput id="chmodMode" label="Octal mode" bind:value={dialogValue} placeholder="644" required />
      {:else if d.type === 'rename'}
        <FormInput id="renameName" label="New name" bind:value={dialogValue} required />
      {:else if d.type === 'file'}
        <FormInput id="newFileName" label="File name" bind:value={dialogValue} placeholder="config.yml" required />
      {:else}
        <FormInput id="newFolderName" label="Folder name" bind:value={dialogValue} placeholder="plugins" required />
      {/if}
      <div class="flex justify-end">
        <button type="submit" class="button button-highlighted">
          {d.type === 'file' ? 'Create file' : d.type === 'folder' ? 'Create folder' : d.type === 'chmod' ? 'Apply' : 'Rename'}
        </button>
      </div>
    </form>
  </Modal>
{/if}

{#if editorOpen}
  <Modal title="Edit file" isLarge focusFirstField={false} onclose={() => (editorOpen = false)}>
    <form onsubmit={saveEditor} class="flex flex-col gap-4">
      <div class="flex items-center justify-between gap-2">
        <p class="truncate font-mono text-xs text-coollabs-subtle">{editingPath}</p>
        {#if editorLanguage}
          <span class="shrink-0 rounded-md bg-coollabs/10 px-1.5 py-0.5 font-mono text-[11px] text-coollabs dark:bg-white/[0.08] dark:text-neutral-200">{editorLanguage}</span>
        {/if}
      </div>
      <div class="min-w-0 file-editor-monaco">
        <MonacoEditor bind:value={editorContent} language={editorLanguage} />
      </div>
      <div class="flex justify-end gap-2">
        <button type="button" class="button" onclick={() => (editorOpen = false)}>Cancel</button>
        <button type="submit" class="button button-highlighted">Save changes</button>
      </div>
    </form>
  </Modal>
{/if}
