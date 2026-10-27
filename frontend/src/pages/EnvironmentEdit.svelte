<script lang="ts">
  // Port of upstream livewire/project/environment-edit.blade.php with
  // livewire/project/delete-environment.blade.php.
  import Reicon from '../components/Reicon.svelte';
  import Empty from '../components/Empty.svelte';
  import FormInput from '../components/FormInput.svelte';
  import UnsavedBar from '../components/UnsavedBar.svelte';
  import ConfirmModal from '../components/ConfirmModal.svelte';
  import { api, uiApi, ApiError, type ProjectPageData } from '../lib/api';
  import { link, router } from '../lib/router.svelte';
  import { setProjectCrumbs } from '../lib/crumbs';
  import { notify } from '../lib/toast.svelte';

  let { uuid, env }: { uuid: string; env: string } = $props();

  let data = $state<ProjectPageData | null>(null);
  let missing = $state(false);
  let name = $state('');
  let description = $state('');
  let saved = $state({ name: '', description: '' });
  let saving = $state(false);
  let error = $state<ApiError | null>(null);
  let deleting = $state(false);

  const environment = $derived(data?.environments.find((e) => e.uuid === env));

  async function load() {
    try {
      const d = await uiApi.project(uuid);
      const e = d.environments.find((x) => x.uuid === env);
      if (!e) {
        missing = true;
        return;
      }
      data = d;
      setProjectCrumbs(d, env);
      saved = { name: e.name, description: e.description ?? '' };
      name = saved.name;
      description = saved.description;
      document.title = `${e.name.slice(0, 10)} > Edit | coolify-rs`;
    } catch (e) {
      if (e instanceof ApiError && e.status === 404) missing = true;
      else throw e;
    }
  }
  $effect(() => {
    void env;
    load();
  });

  const dirty = $derived(!!data && (name !== saved.name || description !== saved.description));
  const base = $derived(`/projects/${encodeURIComponent(uuid)}/environments/${encodeURIComponent(env)}`);

  async function save() {
    saving = true;
    error = null;
    try {
      await api.patch(base, { name, description: description.trim() ? description : null });
      await load();
    } catch (e) {
      error = e instanceof ApiError ? e : new ApiError(0, String(e));
      if (!Object.keys(error.errors).length) notify.error(error.message);
    } finally {
      saving = false;
    }
  }

  async function remove() {
    await api.del(base);
    router.go(`/project/${uuid}`);
  }
</script>

{#if missing}
  <div class="application-settings-form w-full">
    <Empty title="Environment not found" description="It may have been deleted, or it belongs to another team." iconName="layers" size="lg" />
  </div>
{:else if data && environment}
  <div>
    <div class="w-full max-w-none">
      <header class="mb-5 flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
        <div class="min-w-0">
          <h1 class="truncate text-[24px]! leading-7! font-semibold! tracking-tight!">{environment.name}</h1>
          <p class="mt-1 text-[13px] text-neutral-500 dark:text-fg-dim">Environment settings in {data.project.name}</p>
        </div>
        {#if data.can_create}
          <div class="flex w-fit shrink-0 items-center gap-2">
            <a class="button whitespace-nowrap" use:link href="/project/{uuid}/environment/{env}/clone">
              <Reicon name="layers" class="size-3.5 opacity-70" />
              Clone environment
            </a>
          </div>
        {/if}
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
                <h2>Environment details</h2>
                <p>Name and describe this environment inside {data.project.name}.</p>
              </div>
            </div>
            <div class="application-settings-section-body grid gap-4 sm:grid-cols-2">
              <FormInput label="Name" id="name" bind:value={name} disabled={!data.can_update} error={error?.field('name')} />
              <FormInput
                label="Description"
                id="description"
                bind:value={description}
                disabled={!data.can_update}
                error={error?.field('description')}
              />
            </div>
          </section>
        </form>
        {#if data.can_update}
          <section class="overflow-hidden rounded-[10px] border border-red-300 bg-red-50/80 dark:border-red-500/25 dark:bg-red-500/[0.06]">
            <div class="flex flex-col gap-4 px-5 py-4 sm:flex-row sm:items-start sm:justify-between">
              <div class="min-w-0">
                <h2 class="text-sm font-semibold text-red-800 dark:text-red-300">Delete environment</h2>
                <p class="mt-1 max-w-2xl text-sm text-red-700/80 dark:text-red-200/70">
                  Remove every resource before permanently deleting this environment.
                </p>
              </div>
              <div class="shrink-0 sm:pt-0.5">
                <div class="relative h-auto max-w-full inline-flex w-auto">
                  <button type="button" class="button button-error" disabled={environment.resources > 0} onclick={() => (deleting = true)}>
                    Delete
                  </button>
                </div>
              </div>
            </div>
          </section>
        {/if}
      </div>
    </div>
  </div>
{/if}

{#if deleting && environment}
  <ConfirmModal
    title="Confirm Environment Deletion?"
    actions={['This will delete the selected environment.']}
    confirmationLabel="Please confirm the execution of the actions by entering the Environment Name below"
    shortConfirmationLabel="Environment Name"
    confirmationText={environment.name}
    step2ButtonText="Permanently Delete"
    onconfirm={remove}
    onclose={() => (deleting = false)}
  />
{/if}
