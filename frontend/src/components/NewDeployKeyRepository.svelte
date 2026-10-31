<script lang="ts">
  // Port of upstream livewire/project/new/github-private-repository-deploy-key.blade.php
  // (+ Project\New\GithubPrivateRepositoryDeployKey).
  import Empty from './Empty.svelte';
  import FormInput from './FormInput.svelte';
  import PrivateBuildConfig from './PrivateBuildConfig.svelte';
  import Reicon from './Reicon.svelte';
  import { ApiError } from '../lib/api';
  import { link, router } from '../lib/router.svelte';
  import { notify } from '../lib/toast.svelte';
  import { privateRepoUi } from '../lib/sourcesApi';

  let { project, env, destination }: { project: string; env: string; destination: string } = $props();

  let keys = $state<{ id: number; name: string; description?: string | null }[] | null>(null);
  let keyId = $state<number | null>(null);
  let repositoryUrl = $state('');
  let branch = $state('');
  let baseDirectory = $state('');
  let submitting = $state(false);
  let errors = $state<Record<string, string[]>>({});
  let config: PrivateBuildConfig | undefined = $state();

  $effect(() => {
    privateRepoUi
      .keys()
      .then((r) => (keys = r.keys))
      .catch((e) => notify.failure(e instanceof ApiError ? e.message : String(e)));
  });

  /** The repository of `get_git_source()`, as `applicationForDetection()` sees it. */
  function scan() {
    return privateRepoUi.scan('deploy-key', {
      private_key_id: keyId,
      repository: repositoryUrl,
      git_branch: branch || 'main',
      base_directory: baseDirectory || '/',
      destination_uuid: destination,
    });
  }

  async function submit() {
    if (submitting || !config) return;
    submitting = true;
    errors = {};
    try {
      const { uuid } = await privateRepoUi.create('deploy-key', {
        ...config.values(),
        project_uuid: project,
        environment_uuid: env,
        destination_uuid: destination,
        private_key_id: keyId,
        repository_url: repositoryUrl,
      });
      router.go(`/project/${project}/environment/${env}/application/${uuid}`);
    } catch (e) {
      if (e instanceof ApiError) {
        errors = e.errors;
        notify.failure(e.message);
      } else notify.failure(String(e));
      submitting = false;
    }
  }
</script>

{#if keys}
  <div class="mt-8 flex w-full max-w-none flex-col gap-6 lg:mt-3">
    {#if keyId === null}
      <section class="application-settings-section">
        <div class="application-settings-section-header">
          <div>
            <h2>Private repository</h2>
            <p>Choose the SSH key Coolify will use to clone this repository.</p>
          </div>
        </div>
        <div class="application-settings-section-body p-0!">
          {#each keys as key (key.id)}
            <button
              type="button"
              class="group flex w-full items-center gap-3 border-b border-neutral-200 px-4 py-3 text-left transition-colors last:border-b-0 hover:bg-neutral-50 dark:border-white/[0.06] dark:hover:bg-white/[0.025]"
              onclick={() => (keyId = key.id)}
            >
              <div class="flex size-9 shrink-0 items-center justify-center rounded-lg bg-neutral-100 text-neutral-500 dark:bg-white/[0.06] dark:text-fg-dim">
                <Reicon name="keys" class="size-4" />
              </div>
              <div class="min-w-0 flex-1">
                <div class="truncate text-sm font-semibold text-black dark:text-fg">{key.name}</div>
                <p class="mt-0.5 truncate text-xs text-neutral-500 dark:text-fg-dim">{key.description || 'SSH deploy key'}</p>
              </div>
            </button>
          {:else}
            <Empty title="No private keys" description="Create an SSH key before connecting a private repository." iconName="keys">
              {#snippet contents()}
                <a class="button" href="/security/private-key" use:link>Create private key</a>
              {/snippet}
            </Empty>
          {/each}
        </div>
      </section>
    {:else}
      <PrivateBuildConfig
        bind:this={config}
        deployKey
        freeBranch
        title="Repository configuration"
        description="Enter the repository location and choose how Coolify should build it."
        {submitting}
        bind:branch
        bind:baseDirectory
        onsubmit={submit}
        onscan={scan}
      >
        {#snippet head()}
          <FormInput id="repository_url" required label="Repository URL" placeholder="git@github.com:owner/repository.git" bind:value={repositoryUrl} error={errors.repository_url?.[0]} />
        {/snippet}
      </PrivateBuildConfig>
    {/if}
  </div>
{/if}
