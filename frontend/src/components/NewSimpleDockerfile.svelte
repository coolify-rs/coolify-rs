<script lang="ts">
  // Port of upstream livewire/project/new/simple-dockerfile.blade.php (+ Project\New\SimpleDockerfile).
  import MonacoEditor from './MonacoEditor.svelte';
  import { ApiError } from '../lib/api';
  import { router } from '../lib/router.svelte';
  import { notify } from '../lib/toast.svelte';
  import { privateRepoUi } from '../lib/sourcesApi';

  let { project, env, destination }: { project: string; env: string; destination: string } = $props();

  let dockerfile = $state('');
  let saving = $state(false);

  async function submit(e: SubmitEvent) {
    e.preventDefault();
    if (saving) return;
    saving = true;
    try {
      const { uuid } = await privateRepoUi.dockerfile({ project_uuid: project, environment_uuid: env, destination_uuid: destination, dockerfile });
      router.go(`/project/${project}/environment/${env}/application/${uuid}`);
    } catch (err) {
      notify.failure(err instanceof ApiError ? err.message : String(err));
      saving = false;
    }
  }
</script>

<div class="mt-8 w-full max-w-none lg:mt-3">
  <form onsubmit={submit}>
    <section class="application-settings-section">
      <div class="application-settings-section-header">
        <div>
          <h2>Dockerfile</h2>
          <p>Create an application directly from a Dockerfile without connecting a Git repository.</p>
        </div>
        <button type="submit" class="button button-highlighted" disabled={saving}>Create application</button>
      </div>
      <div class="application-settings-section-body p-0!">
        <MonacoEditor language="dockerfile" rows={20} bind:value={dockerfile} />
      </div>
    </section>
  </form>
</div>
