<script lang="ts">
  // Port of upstream livewire/deployments-indicator.blade.php (variant `topbar`).
  import { onDestroy } from 'svelte';
  import Reicon from './Reicon.svelte';
  import StatusBadge from './StatusBadge.svelte';
  import { link, router } from '../lib/router.svelte';
  import { get } from '../lib/serverApi';

  type Deployment = {
    id: number;
    application_uuid: string | null;
    application_name: string | null;
    deployment_url: string | null;
    pull_request_id: number;
    server_name: string | null;
    status: string;
    project_name: string | null;
    environment_name: string | null;
  };
  let deployments = $state<Deployment[]>([]);
  let expanded = $state(false);
  let root = $state<HTMLElement>();

  async function load() {
    if (document.hidden) return;
    try {
      deployments = (await get<{ deployments: Deployment[] }>('/deployments-indicator')).deployments;
    } catch {
      // The next poll tries again.
    }
  }
  load();
  const timer = setInterval(load, 3000);
  onDestroy(() => clearInterval(timer));

  // `$currentApplicationUuid`: the route's application, whose heading has its own "Deploying…" indicator.
  const currentApplication = $derived(router.path.match(/\/application\/([^/]+)/)?.[1] ?? null);
  const onlyCurrent = $derived(currentApplication !== null && deployments.every((d) => d.application_uuid === currentApplication));
  const label = $derived(`${deployments.length} ${deployments.length === 1 ? 'deployment' : 'deployments'}`);
  const path = (d: Deployment) => [d.project_name, d.environment_name].filter(Boolean).join(' / ');
</script>

<svelte:window
  onclick={(e) => expanded && root && !root.contains(e.target as Node) && (expanded = false)}
  onkeydown={(e) => e.key === 'Escape' && (expanded = false)}
/>

<div class="relative" bind:this={root}>
  {#if deployments.length > 0 && !onlyCurrent}
    {#if expanded}
      <div class="surface-popover absolute top-full z-60 mt-2 overflow-hidden rounded-xl right-0 w-[22rem]">
        <div class="max-h-96 space-y-1 overflow-y-auto p-2 scrollbar">
          {#each deployments as d (d.id)}
            <a
              href={d.deployment_url ?? '#'}
              use:link
              class="flex items-start gap-3 rounded-lg border border-transparent p-3 transition-colors hover:border-neutral-200 hover:bg-neutral-50 hover:no-underline dark:border-coolgray-300 dark:hover:border-coolgray-400 dark:hover:bg-raised"
            >
              <div class="mt-0.5 flex size-8 shrink-0 items-center justify-center rounded-lg border border-neutral-200 bg-neutral-50 text-coollabs dark:border-coolgray-300 dark:bg-raised dark:text-brand-light">
                {#if d.status === 'in_progress'}
                  <svg class="size-3.5 animate-spin" xmlns="http://www.w3.org/2000/svg" fill="none" viewBox="0 0 24 24" aria-hidden="true">
                    <circle class="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" stroke-width="4"></circle>
                    <path class="opacity-75" fill="currentColor" d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4zm2 5.291A7.962 7.962 0 014 12H0c0 3.042 1.135 5.824 3 7.938l3-2.647z"></path>
                  </svg>
                {:else}
                  <Reicon name="time-back" class="size-3.5 text-neutral-500 dark:text-fg-dim" />
                {/if}
              </div>
              <div class="min-w-0 flex-1">
                <div class="flex items-start justify-between gap-2">
                  <p class="truncate text-[13px] font-semibold text-black dark:text-fg">{d.application_name}</p>
                  <StatusBadge label={d.status === 'in_progress' ? 'In progress' : 'Queued'} type={d.status === 'in_progress' ? 'warning' : 'neutral'} class="shrink-0" />
                </div>
                {#if path(d) !== ''}
                  <p class="mt-0.5 truncate text-[11px] text-neutral-500 dark:text-fg-faint">{path(d)}</p>
                {/if}
                <p class="mt-0.5 truncate text-[11px] text-neutral-500 dark:text-fg-faint">
                  {d.server_name || '-'}
                  {#if d.pull_request_id}
                    <span class="px-1 text-neutral-300 dark:text-fg-faint">·</span>
                    PR #{d.pull_request_id}
                  {/if}
                </p>
              </div>
            </a>
          {/each}
        </div>
      </div>
    {/if}
    <button
      type="button"
      onclick={() => (expanded = !expanded)}
      title={label}
      aria-label="Active deployments"
      aria-expanded={expanded}
      class="flex h-9 items-center gap-1.5 rounded-md px-2 text-[13px] font-medium text-coollabs transition-colors hover:bg-neutral-100 dark:text-brand-light dark:hover:bg-white/[0.06] {expanded ? 'bg-neutral-100 dark:bg-white/[0.06]' : ''}"
    >
      <svg class="size-3.5 shrink-0 animate-spin" xmlns="http://www.w3.org/2000/svg" fill="none" viewBox="0 0 24 24" aria-hidden="true">
        <circle class="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" stroke-width="4"></circle>
        <path class="opacity-75" fill="currentColor" d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4zm2 5.291A7.962 7.962 0 014 12H0c0 3.042 1.135 5.824 3 7.938l3-2.647z"></path>
      </svg>
      <span>{deployments.length}</span>
    </button>
  {/if}
</div>
