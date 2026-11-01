<script lang="ts">
  // Port of upstream livewire/project/edit.blade.php with
  // livewire/project/delete-project.blade.php. coolify-rs has no project
  import Reicon from '../components/Reicon.svelte';
  import { compressAvatar, uploadImage } from '../lib/teamsApi';
  import Empty from '../components/Empty.svelte';
  import FormInput from '../components/FormInput.svelte';
  import UnsavedBar from '../components/UnsavedBar.svelte';
  import ConfirmModal from '../components/ConfirmModal.svelte';
  import { api, csrf, request, uiApi, ApiError, type ProjectPageData } from '../lib/api';
  import { router } from '../lib/router.svelte';
  import { setProjectCrumbs } from '../lib/crumbs';
  import { notify } from '../lib/toast.svelte';

  let { uuid }: { uuid: string } = $props();

  let data = $state<ProjectPageData | null>(null);
  let missing = $state(false);
  let name = $state('');
  let description = $state('');
  let saved = $state({ name: '', description: '' });
  let saving = $state(false);
  let error = $state<ApiError | null>(null);
  let deleting = $state(false);

  // Icon: `Project\Edit::uploadIcon`, compressed in the browser like upstream's `prepareIcon`.
  let iconUrl = $state<string | null>(null);
  let iconPreview = $state<string | null>(null);
  let iconBusy = $state(false);
  let iconError = $state<string | null>(null);
  let iconInput = $state<HTMLInputElement | undefined>();

  async function prepareIcon(e: Event) {
    const file = (e.currentTarget as HTMLInputElement).files?.[0];
    if (!file) return;
    iconBusy = true;
    iconError = null;
    let jpeg: Blob;
    try {
      jpeg = await compressAvatar(file);
    } catch {
      iconBusy = false;
      iconError = 'The image could not be processed in this browser.';
      return;
    }
    iconPreview = URL.createObjectURL(jpeg);
    try {
      const r = await uploadImage(`/ui/projects/${encodeURIComponent(uuid)}/icon`, jpeg, csrf());
      notify.success(r.message);
      await load();
    } catch {
      iconError = 'The image could not be uploaded.';
    } finally {
      if (iconPreview) URL.revokeObjectURL(iconPreview);
      iconPreview = null;
      iconBusy = false;
      if (iconInput) iconInput.value = '';
    }
  }

  async function removeIcon() {
    const r = await request<{ message: string }>('DELETE', `/ui/projects/${encodeURIComponent(uuid)}/icon`);
    notify.success(r.message);
    await load();
  }

  async function load() {
    try {
      const d = await uiApi.project(uuid);
      data = d;
      setProjectCrumbs(d);
      iconUrl = (d.project as { icon_url?: string | null }).icon_url ?? null;
      saved = { name: d.project.name, description: d.project.description ?? '' };
      name = saved.name;
      description = saved.description;
      document.title = `${d.project.name.slice(0, 10)} > Edit | coolify-rs`;
    } catch (e) {
      if (e instanceof ApiError && e.status === 404) missing = true;
      else throw e;
    }
  }
  $effect(() => {
    void uuid;
    load();
  });

  const dirty = $derived(!!data && (name !== saved.name || description !== saved.description));
  const empty = $derived(!!data && data.environments.every((e) => e.resources === 0));

  async function save() {
    saving = true;
    error = null;
    try {
      await api.patch(`/projects/${encodeURIComponent(uuid)}`, { name, description: description.trim() ? description : null });
      notify.ok('Project updated.');
      await load();
    } catch (e) {
      error = e instanceof ApiError ? e : new ApiError(0, String(e));
      if (!Object.keys(error.errors).length) notify.error(error.message);
    } finally {
      saving = false;
    }
  }

  async function remove() {
    await api.del(`/projects/${encodeURIComponent(uuid)}`);
    router.go('/projects');
  }
</script>

{#if missing}
  <div class="application-settings-form w-full">
    <Empty title="Project not found" description="It may have been deleted, or it belongs to another team." iconName="projects" size="lg" />
  </div>
{:else if data}
  <div>
    <div class="w-full max-w-none">
      <header class="mb-5">
        <h1 class="truncate text-[24px]! leading-7! font-semibold! tracking-tight!">{data.project.name}</h1>
        <p class="mt-1 text-[13px] text-neutral-500 dark:text-fg-dim">Project settings</p>
      </header>
      <div class="flex flex-col gap-6">
        <form
          onsubmit={(e) => {
            e.preventDefault();
            save();
          }}
        >
          <UnsavedBar {dirty} {saving} onsave={save} onreset={() => ((name = saved.name), (description = saved.description), (error = null))} />
          <section class="application-settings-section">
            <div class="application-settings-section-header">
              <div>
                <h2>Project details</h2>
                <p>Icon, name, and description shown across the dashboard.</p>
              </div>
            </div>
            <div class="application-settings-section-body grid gap-4 sm:grid-cols-2">
              <div class="sm:col-span-2">
                <div class="mb-2 text-sm font-medium leading-4">Icon</div>
                <div class="flex items-center gap-4">
                  <div class="flex size-16 shrink-0 items-center justify-center overflow-hidden rounded-xl bg-neutral-200 text-neutral-700 dark:bg-white/[0.1] dark:text-fg">
                    {#if iconPreview}
                      <img src={iconPreview} alt="Project icon preview" class="h-full w-full object-cover" />
                    {:else if iconUrl}
                      <img src={iconUrl} alt="{name} icon" class="h-full w-full object-cover" />
                    {:else}
                      <Reicon name="projects" class="size-6" />
                    {/if}
                  </div>
                  <div class="flex min-w-0 flex-1 flex-col gap-2">
                    <div class="flex flex-wrap gap-2">
                      <input bind:this={iconInput} type="file" onchange={prepareIcon} accept="image/jpeg,image/png,image/webp" class="hidden" />
                      <button type="button" class="button" disabled={iconBusy} onclick={() => iconInput?.click()}>{iconBusy ? 'Uploading…' : 'Browse…'}</button>
                      {#if iconUrl}
                        <button type="button" class="button button-error" disabled={iconBusy} onclick={removeIcon}>Remove</button>
                      {/if}
                    </div>
                    <p class="text-xs text-neutral-500 dark:text-fg-dim">JPG, PNG, or WebP. Shown in the projects list, on the dashboard, and in resource headers.</p>
                    {#if iconError}<p class="text-xs text-red-500">{iconError}</p>{/if}
                  </div>
                </div>
              </div>
              <FormInput label="Name" id="name" bind:value={name} error={error?.field('name')} />
              <FormInput label="Description" id="description" bind:value={description} error={error?.field('description')} />
            </div>
          </section>
        </form>
        {#if data.can_update}
          <section class="overflow-hidden rounded-[10px] border border-red-300 bg-red-50/80 dark:border-red-500/25 dark:bg-red-500/[0.06]">
            <div class="flex items-start justify-between gap-4 px-5 py-4">
              <div>
                <h2 class="text-sm font-semibold text-red-800 dark:text-red-300">Delete project</h2>
                <p class="mt-1 max-w-2xl text-sm text-red-700/80 dark:text-red-200/70">Empty the project before permanently deleting it.</p>
              </div>
              <div class="relative h-auto max-w-full inline-flex w-auto">
                <button type="button" class="button button-error" disabled={!empty} onclick={() => (deleting = true)}>Delete Project</button>
              </div>
            </div>
          </section>
        {/if}
      </div>
    </div>
  </div>
{/if}

{#if deleting && data}
  <ConfirmModal
    title="Confirm Project Deletion?"
    actions={['This will delete the selected project', 'All Environments inside the project will be deleted as well.']}
    confirmationLabel="Please confirm the execution of the actions by entering the Project Name below"
    shortConfirmationLabel="Project Name"
    confirmationText={data.project.name}
    step2ButtonText="Permanently Delete"
    onconfirm={remove}
    onclose={() => (deleting = false)}
  />
{/if}
