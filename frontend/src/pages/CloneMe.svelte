<script lang="ts">
  // Port of upstream livewire/project/clone-me.blade.php (+ Project\CloneMe).
  import FormInput from '../components/FormInput.svelte';
  import StatusBadge from '../components/StatusBadge.svelte';
  import Forbidden from '../components/Forbidden.svelte';
  import { ApiError, request, uiApi } from '../lib/api';
  import { setProjectCrumbs } from '../lib/crumbs';
  import { router } from '../lib/router.svelte';
  import { notify } from '../lib/toast.svelte';

  let { uuid, env }: { uuid: string; env: string } = $props();

  interface Row {
    name: string;
    description: string | null;
  }
  interface Data {
    can_update: boolean;
    project_name: string;
    environment_name: string;
    new_name: string;
    servers: { id: number; name: string; destinations: { uuid: string; name: string }[] }[];
    applications: Row[];
    databases: Row[];
    services: Row[];
  }

  let data = $state<Data | null>(null);
  let newName = $state('');
  let selected = $state<string | null>(null);
  let error = $state<ApiError | null>(null);
  let forbidden = $state(false);

  const path = $derived(`/ui/projects/${encodeURIComponent(uuid)}/environments/${encodeURIComponent(env)}/clone`);

  $effect(() => {
    if (!data) load();
  });

  async function load() {
    const d = await request<Data>('GET', path);
    if (!d.can_update) {
      forbidden = true;
      return;
    }
    data = d;
    newName = d.new_name;
    uiApi.project(uuid).then((p) => setProjectCrumbs(p, env));
    document.title = `${d.project_name.length > 10 ? `${d.project_name.slice(0, 10)}...` : d.project_name} > Clone | coolify-rs`;
  }

  const plural = (n: number, word: string) => `${n} ${word}${n === 1 ? '' : 's'}`;
  const destinationCount = $derived((data?.servers ?? []).reduce((n, s) => n + s.destinations.length, 0));
  const resourceCount = $derived(data ? data.applications.length + data.databases.length + data.services.length : 0);
  const groups = $derived<[string, Row[]][]>(
    data
      ? [
          ['Application', data.applications],
          ['Database', data.databases],
          ['Service', data.services],
        ]
      : [],
  );

  // `selectServer()`: clicking the selected row clears it.
  function select(destination: string) {
    selected = selected === destination ? null : destination;
  }

  async function clone(type: 'environment' | 'project') {
    error = null;
    try {
      const r = await request<{ redirect: string }>('POST', path, { type, new_name: newName, selected_destination: selected });
      router.go(r.redirect);
    } catch (err) {
      const f = err instanceof ApiError ? err : new ApiError(0, String(err));
      if (Object.keys(f.errors).length) error = f;
      else notify.failure(f.message);
    }
  }
</script>

{#if forbidden}
  <Forbidden />
{:else if data}
  <div>
    <div class="w-full max-w-none">
      <header class="mb-5">
        <h1 class="truncate text-[24px]! leading-7! font-semibold! tracking-tight!">{data.environment_name}</h1>
        <p class="mt-1 text-[13px] text-neutral-500 dark:text-fg-dim">Clone this environment inside {data.project_name}</p>
      </header>

      <div class="flex flex-col gap-6">
        <section class="application-settings-section">
          <div class="application-settings-section-header">
            <div>
              <h2>Clone environment</h2>
              <p>Copy every resource from {data.environment_name} to a new project or environment.</p>
            </div>
          </div>
          <div class="application-settings-section-body">
            <div class="max-w-md">
              <FormInput required id="newName" label="New name" bind:value={newName} error={error?.errors.newName?.[0]} />
            </div>
          </div>
        </section>

        <section class="application-settings-section">
          <div class="application-settings-section-header">
            <div>
              <h2>Destination</h2>
              <p>Choose the server and Docker network that will receive the cloned resources.</p>
            </div>
          </div>
          <div class="application-settings-section-body p-0!">
            <div class="data-table">
              <div class="data-table-header clone-destinations-table-grid">
                <span><span class="sr-only">Selected</span></span>
                <span>Server</span>
                <span>Network</span>
              </div>
              {#each data.servers as server (server.id)}
                {#each server.destinations as destination (destination.uuid)}
                  <button
                    type="button"
                    onclick={() => select(destination.uuid)}
                    class={[
                      'data-table-row clone-destinations-table-grid w-full border-b border-neutral-200 text-left last:border-b-0 dark:border-white/[0.06]',
                      selected === destination.uuid && 'bg-coollabs/5 dark:bg-brand/[0.08]',
                    ]}
                  >
                    <span
                      class={[
                        'flex size-4 items-center justify-center rounded-full border',
                        selected === destination.uuid
                          ? 'border-coollabs bg-coollabs text-white dark:border-brand dark:bg-brand dark:text-white'
                          : 'border-neutral-300 dark:border-white/[0.15]',
                      ]}
                    >
                      {#if selected === destination.uuid}
                        <span class="size-1.5 rounded-full bg-current"></span>
                      {/if}
                    </span>
                    <span class="truncate text-[12px] font-semibold text-black dark:text-fg">{server.name}</span>
                    <span class="truncate font-mono text-[11px] text-neutral-600 dark:text-fg-dim">{destination.name}</span>
                  </button>
                {/each}
              {/each}
              <div class="flex min-h-11 items-center border-t border-neutral-200 px-4 text-[11px] text-neutral-500 dark:border-white/[0.08] dark:text-fg-faint">
                {plural(destinationCount, 'destination')}
              </div>
            </div>
          </div>
        </section>

        <section class="application-settings-section">
          <div class="application-settings-section-header">
            <div>
              <h2>Resources</h2>
              <p>{plural(resourceCount, 'resource')} will be cloned.</p>
            </div>
          </div>
          <div class="application-settings-section-body p-0!">
            <div class="data-table">
              <div class="data-table-header clone-resources-table-grid">
                <span>Name</span>
                <span>Type</span>
                <span>Description</span>
              </div>
              {#each groups as [type, rows] (type)}
                {#each rows as row, i (`${type}-${i}`)}
                  <div class="data-table-row clone-resources-table-grid border-b border-neutral-200 last:border-b-0 dark:border-white/[0.06]">
                    <div class="truncate text-[12px] font-semibold text-black dark:text-fg">{row.name}</div>
                    <div><StatusBadge label={type} type="neutral" /></div>
                    <div class="truncate text-[11px] text-neutral-600 dark:text-fg-dim">{row.description || '-'}</div>
                  </div>
                {/each}
              {/each}
              <div class="flex min-h-11 items-center border-t border-neutral-200 px-4 text-[11px] text-neutral-500 dark:border-white/[0.08] dark:text-fg-faint">
                {plural(resourceCount, 'resource')}
              </div>
            </div>
            <div class="flex flex-col gap-2 border-t border-neutral-200 p-4 sm:flex-row sm:justify-end dark:border-white/[0.06]">
              <button type="button" class="button button-highlighted" disabled={!selected} onclick={() => clone('environment')}>Clone to environment</button>
              <button type="button" class="button button-highlighted" disabled={!selected} onclick={() => clone('project')}>Clone to project</button>
            </div>
          </div>
        </section>
      </div>
    </div>
  </div>
{/if}
