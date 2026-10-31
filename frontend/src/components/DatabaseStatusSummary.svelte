<script lang="ts">
  // Port of upstream components/status-summary.blade.php (the status chip and
  // its container/healthcheck menu) as livewire/project/database/status.blade.php
  // uses it, with components/application/restart-limit-warning.blade.php.
  import Reicon from './Reicon.svelte';
  import Helper from './Helper.svelte';
  import StatusBadge from './StatusBadge.svelte';
  import { link } from '../lib/router.svelte';

  let {
    status,
    title = 'Database status',
    containerName = 'Container',
    healthcheckUrl,
    restartLimit = null,
  }: { status: string; title?: string; containerName?: string; healthcheckUrl?: string; restartLimit?: number | null } = $props();

  let open = $state(false);
  let root: HTMLDivElement | undefined = $state();

  const headline = (s: string) =>
    s
      .replace(/[_-]+/g, ' ')
      .replace(/([a-z])([A-Z])/g, '$1 $2')
      .trim()
      .replace(/\s+/g, ' ')
      .replace(/\b\w/g, (c) => c.toUpperCase());

  const parsed = $derived.by(() => {
    const raw = (status ?? '').toLowerCase().trim();
    const parts = raw.split(':');
    let container = parts[0] || 'unknown';
    let health: string | null = parts[1] ?? null;
    const excluded = parts.includes('excluded');
    const m = health === null ? raw.match(/^([^()]+)\(([^)]+)\)$/) : null;
    if (m) {
      container = m[1].trim();
      health = m[2].trim();
    }
    const containerLabel = headline(container || 'unknown');
    const healthLabel = excluded
      ? 'Monitoring disabled'
      : health === 'healthy'
        ? 'Healthy'
        : health === 'unhealthy'
          ? 'Unhealthy'
          : health === 'starting'
            ? 'Starting'
            : 'Not configured';
    const containerType = container.startsWith('running')
      ? 'success'
      : ['starting', 'restarting', 'degraded'].some((p) => container.startsWith(p))
        ? 'warning'
        : 'error';
    const healthType = excluded ? 'warning' : health === 'healthy' ? 'success' : health === 'unhealthy' ? 'error' : 'warning';
    let summary: [string, string];
    if (containerType === 'error') summary = [containerLabel, 'error'];
    else if (container.startsWith('degraded')) summary = ['Degraded', 'warning'];
    else if (healthType === 'error') summary = ['Degraded', 'error'];
    else if (containerType === 'warning') summary = [containerLabel, 'warning'];
    else if (excluded) summary = [`${containerLabel} (monitoring disabled)`, 'warning'];
    else if (health === 'starting') summary = [`${containerLabel} (healthcheck starting)`, 'warning'];
    else if (healthType === 'warning') summary = [`${containerLabel} (no healthcheck)`, 'warning'];
    else summary = [containerLabel, 'success'];
    return { containerLabel, healthLabel, containerType, healthType, summary };
  });

  const dot: Record<string, string> = { success: 'bg-success', warning: 'bg-warning', error: 'bg-error' };
  const healthHelper =
    'No healthcheck is configured, so Coolify can only report the container state. Traffic can still be routed to the container, but Coolify cannot verify that the application inside it is ready to receive requests.';
</script>

<svelte:window
  onclick={(e) => open && root && !root.contains(e.target as Node) && (open = false)}
  onkeydown={(e) => e.key === 'Escape' && (open = false)}
/>

<div class="flex items-center gap-1">
  <div class="relative shrink-0" bind:this={root}>
    <button
      type="button"
      onclick={() => (open = !open)}
      aria-expanded={open}
      aria-haspopup="menu"
      class="inline-flex h-6 max-w-full items-center gap-1.5 whitespace-nowrap rounded-full border border-neutral-200 bg-neutral-100 px-2 text-xs font-medium leading-none text-neutral-700 dark:border-white/[0.12] dark:bg-white/[0.07] dark:text-white transition-colors cursor-pointer"
    >
      <span class="size-1.5 shrink-0 rounded-full {dot[parsed.summary[1]]}"></span>
      <span>{parsed.summary[0]}</span>
      <span class="inline-flex transition-transform {open ? 'rotate-180' : ''}">
        <Reicon name="chevron-down" class="size-3 opacity-55" />
      </span>
    </button>
    {#if open}
      <div class="listbox-panel top-8! z-[90]! w-[min(16rem,calc(100vw-1.5rem))]! min-w-0! sm:w-64! sm:min-w-64! right-auto! left-0!" role="menu">
        <div class="px-3 py-2 text-[11px] font-medium text-neutral-400 dark:text-fg-faint">{title}</div>
        <div class="listbox-option cursor-default! gap-2.5!">
          <span class="size-1.5 shrink-0 rounded-full {dot[parsed.containerType]}"></span>
          <span class="flex-1">{containerName}</span>
          <span>{parsed.containerLabel}</span>
        </div>
        {#if healthcheckUrl}
          <div class="listbox-option gap-0! p-0!">
            <a href={healthcheckUrl} use:link role="menuitem" class="flex min-h-8 flex-1 items-center gap-2.5 px-2 py-1.5" onclick={() => (open = false)}>
              <span class="size-1.5 shrink-0 rounded-full {dot[parsed.healthType]}"></span>
              <span class="flex-1">Healthcheck</span>
              <span>{parsed.healthLabel}</span>
            </a>
            {#if parsed.healthLabel === 'Not configured'}
              <span class="inline-flex pr-2"><Helper label="About unconfigured healthchecks" helper={healthHelper} /></span>
            {/if}
          </div>
        {:else}
          <div class="listbox-option cursor-default! gap-2.5!">
            <span class="size-1.5 shrink-0 rounded-full {dot[parsed.healthType]}"></span>
            <span class="flex-1">Healthcheck</span>
            <span class="inline-flex items-center gap-1.5">
              {parsed.healthLabel}
              {#if parsed.healthLabel === 'Not configured'}<Helper label="About unconfigured healthchecks" helper={healthHelper} />{/if}
            </span>
          </div>
        {/if}
      </div>
    {/if}
  </div>
  {#if restartLimit !== null}
    <span title="Container has crashed and Coolify stopped it after {restartLimit} restart attempts."
      ><StatusBadge label="Restart limit reached" type="warning" /></span
    >
  {/if}
</div>
