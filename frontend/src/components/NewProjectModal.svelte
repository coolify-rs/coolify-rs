<script lang="ts">
  // Port of upstream livewire/project/add-empty.blade.php inside a modal.
  // Like upstream, a new project opens its production environment.
  import Modal from './Modal.svelte';
  import FormInput from './FormInput.svelte';
  import { api, ApiError, type Project } from '../lib/api';
  import { router } from '../lib/router.svelte';
  import { notify } from '../lib/toast.svelte';

  let { onclose, title }: { onclose: () => void; title?: string } = $props();

  let name = $state('');
  let description = $state('');
  let busy = $state(false);
  let error = $state<ApiError | null>(null);

  async function submit(e: SubmitEvent) {
    e.preventDefault();
    busy = true;
    error = null;
    try {
      const { uuid } = await api.post<{ uuid: string }>('/projects', { name, description: description || null });
      const project = await api.get<Project>(`/projects/${uuid}`);
      const env = project.environments.find((x) => x.name === 'production') ?? project.environments[0];
      onclose();
      router.go(env ? `/project/${uuid}/environment/${env.uuid}` : `/project/${uuid}`);
    } catch (err) {
      error = err instanceof ApiError ? err : new ApiError(0, String(err));
      if (!Object.keys(error.errors).length) notify.error(error.message);
    } finally {
      busy = false;
    }
  }
</script>

<Modal title={title ?? 'New Project'} {onclose}>
  <form class="space-y-4" onsubmit={submit}>
    <div class="grid gap-4 md:grid-cols-2">
      <FormInput label="Name" id="name" placeholder="Your project name" required bind:value={name} error={error?.field('name')} />
      <FormInput
        label="Description"
        id="description"
        placeholder="A short project description"
        bind:value={description}
        error={error?.field('description')}
      />
    </div>
    <p
      class="rounded-lg border border-neutral-200 bg-neutral-50 px-3 py-2.5 text-[12px] text-neutral-500 dark:border-white/[0.08] dark:bg-white/[0.05] dark:text-fg-dim"
    >
      A production environment will be created automatically.
    </p>
    <footer class="flex justify-end border-t border-neutral-200 pt-4 dark:border-white/[0.08]">
      <button class="button button-highlighted" type="submit" disabled={busy}>Create project</button>
    </footer>
  </form>
</Modal>
