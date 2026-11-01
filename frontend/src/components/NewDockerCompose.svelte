<script lang="ts">
  // Port of upstream livewire/project/new/docker-compose.blade.php (+ Project\New\DockerCompose).
  import MonacoEditor from './MonacoEditor.svelte';
  import { ApiError } from '../lib/api';
  import { router } from '../lib/router.svelte';
  import { notify } from '../lib/toast.svelte';
  import { privateRepoUi } from '../lib/sourcesApi';

  let { project, env, destination }: { project: string; env: string; destination: string } = $props();

  let dockerComposeRaw = $state('');
  let saving = $state(false);

  async function submit(e: SubmitEvent) {
    e.preventDefault();
    if (saving) return;
    saving = true;
    try {
      const { uuid } = await privateRepoUi.compose({ project_uuid: project, environment_uuid: env, destination_uuid: destination, docker_compose_raw: dockerComposeRaw });
      router.go(`/project/${project}/environment/${env}/service/${uuid}`);
    } catch (err) {
      notify.failure(err instanceof ApiError ? err.message : String(err));
      saving = false;
    }
  }
</script>

<div class="mt-8 w-full lg:mt-3">
  <form onsubmit={submit}>
    <section class="application-settings-section">
      <div class="application-settings-section-header">
        <div>
          <h2>Docker Compose</h2>
          <p>Create a multi-container service directly from a Compose file.</p>
        </div>
        <button type="submit" class="button button-highlighted" disabled={saving}>Create service</button>
      </div>
      <div class="application-settings-section-body">
        <div class="flex-1 form-control">
          <div class="mb-1.5 flex h-4 w-full items-center gap-1.5">
            <label class="mb-0! flex items-center gap-1 text-sm font-medium leading-4" for="dockerComposeRaw">Docker Compose file</label>
          </div>
          <MonacoEditor language="yaml" rows={20} bind:value={dockerComposeRaw} />
        </div>
      </div>
    </section>
  </form>
</div>
