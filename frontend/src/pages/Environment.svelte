<script lang="ts">
  // Port of upstream livewire/project/resource/index.blade.php. coolify-rs
  // has no resource APIs yet, so an environment with resources says so
  // instead of listing them.
  import Reicon from '../components/Reicon.svelte';
  import Empty from '../components/Empty.svelte';
  import { uiApi, ApiError, type ProjectPageData } from '../lib/api';
  import { link } from '../lib/router.svelte';
  import { setProjectCrumbs } from '../lib/crumbs';

  let { uuid, env }: { uuid: string; env: string } = $props();

  let data = $state<ProjectPageData | null>(null);
  let missing = $state(false);
  const environment = $derived(data?.environments.find((e) => e.uuid === env));

  $effect(() => {
    uiApi
      .project(uuid)
      .then((d) => {
        const e = d.environments.find((x) => x.uuid === env);
        if (!e) {
          missing = true;
          return;
        }
        data = d;
        setProjectCrumbs(d, env);
        document.title = `${e.name.slice(0, 10)} > Resources | coolify-rs`;
      })
      .catch((e) => {
        if (e instanceof ApiError && e.status === 404) missing = true;
        else throw e;
      });
  });
</script>

{#if missing}
  <div class="application-settings-form w-full">
    <Empty title="Environment not found" description="It may have been deleted, or it belongs to another team." iconName="layers" size="lg" />
  </div>
{:else if data && environment}
  <div>
    <div class="w-full">
      <header class="mb-5 flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
        <div class="min-w-0">
          <div class="flex min-w-0 items-center gap-1.5">
            <h1 class="truncate text-[24px]! leading-7! font-semibold! tracking-tight!">{environment.name}</h1>
            {#if data.can_update}
              <a
                href="/project/{uuid}/environment/{env}/edit"
                use:link
                class="flex size-7 shrink-0 items-center justify-center rounded-md text-neutral-400 transition-colors hover:bg-neutral-100 hover:text-black dark:text-fg-faint dark:hover:bg-white/[0.06] dark:hover:text-fg"
                title="Environment settings"
                aria-label="Open settings for {environment.name}"
              >
                <Reicon name="settings" class="size-4" />
              </a>
            {/if}
          </div>
          <p class="mt-1 text-[13px] text-neutral-500 dark:text-fg-dim">
            <span>{environment.resources} {environment.resources === 1 ? 'resource' : 'resources'}</span>
            in
            <a
              href="/project/{uuid}"
              use:link
              class="font-medium text-neutral-700 underline-offset-2 hover:text-black hover:underline dark:text-fg dark:hover:text-white"
              title="Show all environments in {data.project.name}">{data.project.name}</a
            >
          </p>
        </div>
        <div class="flex flex-wrap items-center gap-2 sm:shrink-0">
          <a
            href="/project/{uuid}/environment/{env}/shared-variables"
            use:link
            class="button whitespace-nowrap"
            title="Shared variables for this environment"
            aria-label="Shared variables for {environment.name}"
          >
            <Reicon name="variables" class="size-3.5" />
            Shared variables
          </a>
          {#if data.can_create}
            <a href="/project/{uuid}/environment/{env}/new" use:link class="button whitespace-nowrap button-highlighted">
              <Reicon name="plus" class="size-3.5" />
              New resource
            </a>
          {/if}
        </div>
      </header>

      {#if environment.resources === 0}
        <Empty title="No resources yet" description="Add an application, database, or service to this environment." iconName="layers">
          {#snippet contents()}
            {#if data?.can_create}
              <a href="/project/{uuid}/environment/{env}/new" use:link class="button">
                <Reicon name="plus" class="size-3.5" />
                Add resource
              </a>
            {/if}
          {/snippet}
        </Empty>
      {:else}
        <Empty
          title="Resources are not available in coolify-rs yet"
          description="This environment has resources. Manage them in Coolify for now."
          iconName="layers"
        />
      {/if}
    </div>
  </div>
{/if}
