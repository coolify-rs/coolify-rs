<script lang="ts">
  // Port of upstream livewire/project/new/docker-image.blade.php (+
  // Project\New\DockerImage): the image reference form of the "Docker Image"
  // resource. Submitting creates the application and opens it.
  import FormInput from './FormInput.svelte';
  import { appUi, ApiError } from '../lib/api';
  import { router } from '../lib/router.svelte';

  let { project, env, destination }: { project: string; env: string; destination: string } = $props();

  let imageName = $state('');
  let imageTag = $state('');
  let imageSha256 = $state('');
  let saving = $state(false);
  let error = $state<ApiError | null>(null);
  const err = (k: string) => error?.field(k);

  function autofocus(node: HTMLElement) {
    queueMicrotask(() => node.querySelector('input')?.focus());
  }

  async function submit(e: SubmitEvent) {
    e.preventDefault();
    if (saving) return;
    saving = true;
    error = null;
    try {
      const { uuid } = await appUi.createDockerImage({
        project_uuid: project,
        environment_uuid: env,
        destination_uuid: destination,
        image_name: imageName,
        image_tag: imageTag,
        image_sha256: imageSha256,
      });
      router.go(`/project/${project}/environment/${env}/application/${uuid}`);
    } catch (e) {
      error = e instanceof ApiError ? e : new ApiError(0, String(e));
      saving = false;
    }
  }
</script>

<div class="mt-8 w-full max-w-[920px] lg:mt-3">
  <form onsubmit={submit}>
    <section class="application-settings-section">
      <div class="application-settings-section-header">
        <div>
          <h2>Docker image</h2>
          <p>Deploy an existing image from Docker Hub or another OCI registry.</p>
        </div>
        <button type="submit" class="button button-highlighted" disabled={saving}>Create application</button>
      </div>
      <div class="application-settings-section-body space-y-4">
        <div use:autofocus>
          <FormInput
            id="imageName"
            label="Image name"
            placeholder="nginx, ghcr.io/user/app:v1.2.3, or nginx:stable@sha256:…"
            helper="Paste a complete image reference, or enter a name and use one of the optional fields below."
            required
            bind:value={imageName}
            error={err('image_name')}
          />
        </div>
        <div class="grid gap-3 sm:grid-cols-[minmax(0,1fr)_auto_minmax(0,1fr)] sm:items-end" aria-label="Tag and SHA256 digest are mutually exclusive">
          <FormInput id="imageTag" label="Tag" placeholder="latest" helper="Use a mutable tag such as latest or v1.2.3." bind:value={imageTag} error={err('image_tag')} />
          <div class="flex items-center justify-center text-xs font-semibold text-neutral-400 sm:h-9 dark:text-fg-faint">
            <span>OR</span>
          </div>
          <FormInput
            id="imageSha256"
            label="SHA256 digest"
            placeholder="59e02939b1bf39f16c93138a28727aec…"
            helper="Use the 64-character digest without the sha256: prefix."
            bind:value={imageSha256}
            error={err('image_sha256')}
          />
        </div>
      </div>
    </section>
  </form>
</div>
