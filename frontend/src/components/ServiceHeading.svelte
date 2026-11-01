<script lang="ts">
  // Port of upstream livewire/project/service/heading.blade.php (+ the Heading
  // component), livewire/project/service/status.blade.php in the top bar,
  // components/services/links.blade.php and the configuration-changed warning.
  import { untrack } from 'svelte';
  import Reicon from './Reicon.svelte';
  import StatusBadge from './StatusBadge.svelte';
  import ConfirmModal from './ConfirmModal.svelte';
  import ProcessDialog from './ProcessDialog.svelte';
  import DatabaseTopbar from './DatabaseTopbar.svelte';
  import DatabaseStatusSummary from './DatabaseStatusSummary.svelte';
  import DatabaseActivityMonitor from './DatabaseActivityMonitor.svelte';
  import { ApiError } from '../lib/api';
  import { servicesApi, type ServiceActivity, type ServicePage, type ServicePart } from '../lib/servicesApi';
  import { teleport } from '../lib/databaseMenu';
  import { link } from '../lib/router.svelte';
  import { notify } from '../lib/toast.svelte';

  let {
    data,
    base,
    resources,
    selected = null,
    onrefresh,
  }: {
    data: ServicePage;
    base: string;
    resources: { label: string; href: string; active: boolean }[];
    /** The compose resource of a backups page (`stack_service_uuid`). */
    selected?: ServicePart | null;
    onrefresh: () => Promise<void>;
  } = $props();

  const uuid = $derived(data.service.uuid);
  const serviceStatus = $derived(data.service.status ?? 'exited');
  const displayStatus = $derived(selected?.status ?? data.service.status ?? '');
  const running = $derived(serviceStatus.includes('running'));
  const degraded = $derived(serviceStatus.includes('degraded'));
  const removable = $derived(!!selected && selected.container_present !== false && (selected.status ?? '').startsWith('exited'));
  const statusTitle = $derived(selected ? 'Resource status' : 'Service status');
  const containerName = $derived(selected ? 'Container' : 'Containers');
  const restartLimit = $derived.by(() => {
    if (!selected || !(selected.status ?? '').startsWith('exited') || selected.restart_limit_reached !== true) return null;
    return Math.max(Number(selected.restart_count ?? 0), Number(selected.max_restart_count ?? 0));
  });
  const configurationChanged = $derived(data.configuration_changed && !serviceStatus.includes('exited'));
  const hasMobileWarningSlot = typeof document !== 'undefined' && !!document.getElementById('configuration-warning-hud-slot-mobile');

  let deploying = $state(false);
  let working = $state(false);
  let dialogOpen = $state(false);
  let activity = $state<ServiceActivity | null>(null);
  let confirmRestart = $state(false);
  let confirmStop = $state(false);
  let confirmRemove = $state(false);
  let menuOpen = $state<string | null>(null);
  let linksOpen = $state<string | null>(null);
  let warningOpen = $state<string | null>(null);

  let timer: ReturnType<typeof setTimeout> | undefined;
  let lastActivityId: number | null = null;
  let awaitingNew = false;
  let waits = 0;

  const isRunning = (a: ServiceActivity | null) => !!a && (a.status === 'queued' || a.status === 'in_progress');
  const message = (e: unknown) => (e instanceof ApiError ? e.message : String(e));

  $effect(() => {
    untrack(() => {
      const a = data.activity;
      lastActivityId = a?.id ?? null;
      activity = a;
      if (isRunning(a)) {
        working = true;
        watch();
      }
    });
    return () => clearTimeout(timer);
  });

  async function watch() {
    clearTimeout(timer);
    let s: ServicePage;
    try {
      s = await servicesApi.page(uuid);
    } catch {
      timer = setTimeout(watch, 1500);
      return;
    }
    const a = s.activity;
    if (awaitingNew && (a?.id ?? null) === lastActivityId && waits < 40) {
      waits++;
      timer = setTimeout(watch, 1500);
      return;
    }
    awaitingNew = false;
    activity = a;
    lastActivityId = a?.id ?? null;
    if (isRunning(a)) {
      working = true;
      timer = setTimeout(watch, 1500);
      return;
    }
    working = false;
    deploying = false;
    await onrefresh();
  }

  function follow() {
    awaitingNew = true;
    waits = 0;
    activity = null;
    working = true;
    dialogOpen = true;
    watch();
  }

  /** `reopenDeployment`: a deploy is already running. */
  function reopen() {
    if (working) dialogOpen = true;
    else notify.info('No deployment is currently running.');
  }

  async function run(action: string) {
    if (working) return reopen();
    deploying = true;
    try {
      await servicesApi.action(uuid, action);
      follow();
    } catch (e) {
      deploying = false;
      notify.error(message(e));
    }
  }

  const start = () => run('start');
  const forceDeploy = () => run('force-deploy');
  function restart() {
    notify.info('Gracefully stopping service.<br/><br/>It could take a while depending on the service.');
    return run('restart');
  }
  function pullAndRestart() {
    notify.info('Pulling new images and restarting service.');
    return run('pull-restart');
  }

  async function stop(cleanup: boolean) {
    try {
      await servicesApi.action(uuid, 'stop', { docker_cleanup: cleanup });
    } catch (e) {
      notify.error(message(e));
    }
    await onrefresh();
  }

  async function removeContainer() {
    if (!selected) return;
    try {
      const r = await servicesApi.removeContainer(uuid, selected.uuid);
      notify.success(r.message);
    } catch (e) {
      notify.error(message(e));
    }
    await onrefresh();
  }
</script>

<svelte:window
  onclick={(e) => {
    const t = e.target as HTMLElement;
    if (menuOpen && !t.closest(`#${menuOpen}`)) menuOpen = null;
    if (linksOpen && !t.closest(`[data-links="${linksOpen}"]`)) linksOpen = null;
    if (warningOpen && !t.closest(`[data-warning="${warningOpen}"]`)) warningOpen = null;
  }}
  onkeydown={(e) => e.key === 'Escape' && ((menuOpen = null), (linksOpen = null), (warningOpen = null))}
/>

{#snippet spinner()}
  <span class="inline-flex shrink-0 items-center gap-1.5">
    <svg class="size-3.5 shrink-0 animate-spin" viewBox="0 0 24 24" fill="none" aria-hidden="true">
      <circle class="opacity-25" cx="12" cy="12" r="9" stroke="currentColor" stroke-width="3" />
      <path class="opacity-75" d="M21 12a9 9 0 0 0-9-9" stroke="currentColor" stroke-width="3" stroke-linecap="round" />
    </svg>
  </span>
{/snippet}

<!-- components/deploying-indicator.blade.php -->
{#snippet deployingIndicator(cls: string)}
  <button
    type="button"
    onclick={reopen}
    class="inline-flex shrink-0 items-center gap-1.5 rounded-md px-2 py-1 text-[11px] font-medium ring-1 transition-colors bg-coollabs/10 text-coollabs ring-coollabs/25 hover:bg-coollabs/15 hover:no-underline dark:bg-brand/15 dark:text-brand-light dark:ring-brand-light/25 dark:hover:bg-brand/20 {cls}"
    title="View the running deployment log"
  >
    <svg class="size-3 animate-spin" viewBox="0 0 24 24" fill="none" aria-hidden="true">
      <circle cx="12" cy="12" r="9" stroke="currentColor" stroke-width="2.5" opacity="0.25" />
      <path d="M21 12a9 9 0 0 0-9-9" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" />
    </svg>
    <span>Deploying…</span>
  </button>
{/snippet}

<!-- components/services/links.blade.php -->
{#snippet linksMenu(key: string, compact: boolean)}
  <div class="relative" data-links={key}>
    <button
      type="button"
      onclick={() => (linksOpen = linksOpen === key ? null : key)}
      title="Open service links"
      class={compact
        ? 'inline-flex h-6 shrink-0 items-center gap-1.5 rounded-full border border-neutral-200 bg-neutral-100 px-2 text-xs font-medium leading-none text-neutral-700 dark:border-white/[0.12] dark:bg-white/[0.07] dark:text-white'
        : 'app-tab shrink-0 gap-1'}
    >
      <span class="inline-flex items-center gap-2">
        {#if !compact}<Reicon name="external-link" class="size-3.5 shrink-0 opacity-70" />{/if}
        Links
      </span>
      <span class="inline-flex transition-transform {linksOpen === key ? 'rotate-180' : ''}">
        <Reicon name="chevron-down" class="size-3 opacity-55" />
      </span>
    </button>
    {#if linksOpen === key}
      <div
        role="menu"
        class="listbox-panel max-h-80! overflow-y-auto! {compact
          ? 'top-full! mt-1! w-max! min-w-60! max-w-[min(28rem,calc(100vw-2rem))]!'
          : 'top-full! mt-1! right-0! left-auto! min-w-60! max-w-96!'}"
      >
        {#each data.links as l (l)}
          <a class="listbox-option justify-start! gap-2.5!" target="_blank" rel="noreferrer" href={l}>
            <span class="min-w-0 truncate">{l}</span>
          </a>
        {:else}
          <div class="listbox-option justify-start! cursor-default!">No links available</div>
        {/each}
      </div>
    {/if}
  </div>
{/snippet}

<!-- components/split-action.blade.php -->
{#snippet actions(id: string, mobile: boolean)}
  <div class="split-action relative" {id}>
    {#if removable}
      <button type="button" class="split-action-main" onclick={() => (confirmRemove = true)}>
        <Reicon name="trash" class="size-3.5" />
        <span>Remove container</span>
      </button>
    {:else if running || degraded}
      <button type="button" class="split-action-main" disabled={deploying} onclick={() => (confirmRestart = true)}>
        {#if deploying}{@render spinner()}{:else}<Reicon name="restart" class="size-3.5" />{/if}
        <span>{deploying ? 'Restarting…' : mobile ? 'Restart' : 'Restart current version'}</span>
      </button>
    {:else}
      <button type="button" class="split-action-main" disabled={deploying} onclick={start}>
        {#if deploying}{@render spinner()}{:else}<Reicon name="play-circle" class="size-3.5" />{/if}
        <span>{deploying ? 'Deploying…' : 'Deploy'}</span>
      </button>
    {/if}
    <!-- `$slot->hasActualContent()`: the remove action has no other entries, so no caret. -->
    {#if !removable}
    <button
      type="button"
      class="split-action-caret"
      onclick={() => (menuOpen = menuOpen === id ? null : id)}
      aria-expanded={menuOpen === id}
      aria-haspopup="menu"
      aria-label="More actions"
    >
      <span class="inline-flex transition-transform {menuOpen === id ? 'rotate-180' : ''}">
        <Reicon name="chevron-down" class="size-3" />
      </span>
    </button>
    {/if}
    {#if menuOpen === id && !removable}
      <div class="listbox-panel right-0! left-auto! xl:min-w-60! xl:max-w-96!" role="menu">
        {#if running || degraded}
          {#if running}
            <button type="button" class="listbox-option justify-start! gap-2.5!" role="menuitem" onclick={() => ((menuOpen = null), pullAndRestart())}>
              <Reicon name="refresh" class="size-3.5 opacity-70" />
              {mobile ? 'Restart (pull latest)' : 'Pull latest and restart'}
            </button>
          {/if}
          {#if degraded}
            <button type="button" class="listbox-option justify-start! gap-2.5!" role="menuitem" onclick={() => ((menuOpen = null), forceDeploy())}>
              <Reicon name="refresh" class="size-3.5 opacity-70" />
              Force Restart
            </button>
          {/if}
          <button type="button" class="listbox-option justify-start! gap-2.5!" role="menuitem" onclick={() => ((menuOpen = null), (confirmStop = true))}>
            <Reicon name="stop-circle" class="size-3.5 text-error" />
            Stop
          </button>
        {:else}
          <button type="button" class="listbox-option justify-start! gap-2.5!" role="menuitem" onclick={() => ((menuOpen = null), forceDeploy())}>
            <Reicon name="refresh" class="size-3.5 opacity-70" />
            Force Deploy
          </button>
          <button type="button" class="listbox-option justify-start! gap-2.5!" role="menuitem" onclick={() => ((menuOpen = null), stop(true))}>
            <Reicon name="trash" class="size-3.5 opacity-70" />
            Force Cleanup Containers
          </button>
        {/if}
      </div>
    {/if}
  </div>
{/snippet}

{#snippet missingVariables(id: string, desktop: boolean)}
  <div {id} class="relative">
    <button
      type="button"
      class={desktop ? 'button button-highlighted' : 'button gap-1.5'}
      onclick={() => (menuOpen = menuOpen === id ? null : id)}
      aria-expanded={menuOpen === id}
      aria-haspopup="menu"
    >
      Actions
      <Reicon name="chevron-down" class="size-3 opacity-55" />
    </button>
    {#if menuOpen === id}
      <div class={desktop ? 'listbox-panel top-full! right-0! left-auto! mt-1! w-64! min-w-0!' : 'listbox-panel right-0! left-auto!'} role="menu">
        <div class="listbox-option cursor-default! justify-start! gap-2.5! text-neutral-400! dark:text-fg-faint!" role="menuitem" aria-disabled="true">
          <Reicon name="play-circle" class="size-3.5 opacity-70" />
          <span
            >Deploy (<a href="{base}/environment-variables" use:link class="cursor-pointer underline underline-offset-2">missing required env vars</a>)</span
          >
        </div>
      </div>
    {/if}
  </div>
{/snippet}

<!-- components/configuration-warning.blade.php (no diff: coolify-rs has no configuration diff yet) -->
{#snippet warning(key: string)}
  <div class="relative" data-warning={key}>
    <button
      type="button"
      aria-label="Configuration changes not applied"
      aria-haspopup="dialog"
      aria-expanded={warningOpen === key}
      onclick={() => (warningOpen = warningOpen === key ? null : key)}
      class="flex h-8 items-center justify-center gap-1.5 rounded-lg px-2 text-amber-700 transition-colors hover:bg-amber-100 dark:text-warning dark:hover:bg-warning/10"
    >
      <Reicon name="alert-triangle" class="size-4" />
      <span class="hidden text-xs font-medium lg:inline">Changes pending</span>
    </button>
    {#if warningOpen === key}
      <div
        role="dialog"
        class="surface-popover fixed top-14 left-1/2 z-[1100] w-[calc(100vw-2rem)] max-w-sm -translate-x-1/2 rounded-lg p-3 lg:absolute lg:top-full lg:right-0 lg:left-auto lg:mt-2 lg:translate-x-0"
      >
        <div class="flex items-start gap-2.5">
          <span class="flex size-7 shrink-0 items-center justify-center rounded-md bg-amber-100 text-amber-700 dark:bg-warning/10 dark:text-warning">
            <Reicon name="alert-triangle" class="size-4" />
          </span>
          <div class="min-w-0 flex-1">
            <p class="text-[13px] font-semibold leading-4 text-neutral-950 dark:text-fg">The latest configuration has not been applied</p>
            <p class="mt-0.5 text-[11px] leading-4 text-neutral-600 dark:text-fg-dim">Redeploy to apply.</p>
          </div>
        </div>
      </div>
    {/if}
  </div>
{/snippet}

<div use:teleport={'server-topbar-context'}>
  <DatabaseTopbar label={data.service.name} items={resources} status={displayStatus} {restartLimit} {statusTitle} {containerName} healthcheckUrl="" />
</div>

{#if configurationChanged}
  <div use:teleport={'configuration-warning-hud-slot'}>{@render warning('desktop')}</div>
  {#if hasMobileWarningSlot}
    <div use:teleport={'configuration-warning-hud-slot-mobile'}>{@render warning('mobile')}</div>
  {/if}
{/if}

<nav class="w-full max-w-none">
  <div>
    <!-- Below xl: the title and the resource actions share one row. -->
    <div class="mb-3 flex w-full min-w-0 items-start gap-3 xl:hidden">
      <div class="min-w-0 flex-1">
        <div class="flex min-w-0 flex-col items-start gap-2">
          <h1 class="min-w-0 max-w-full truncate text-[24px]! leading-7! font-semibold! tracking-tight! text-black dark:text-fg">
            {data.service.name}
          </h1>
          <div class="relative flex w-full min-w-0 items-center gap-2">
            <DatabaseStatusSummary status={displayStatus} title={statusTitle} {containerName} />
            {@render linksMenu('mobile', true)}
            {#if working}{@render deployingIndicator('')}{/if}
          </div>
          <div class="flex w-full flex-wrap gap-1">
            {#if restartLimit !== null}
              <span title="Container has crashed and coolify-rs stopped it after {restartLimit} restart attempts."
                ><StatusBadge label="Restart limit reached" type="warning" /></span
              >
            {/if}
          </div>
        </div>
      </div>
      <div class="shrink-0">
        {#if data.can_update}
          {#if data.is_deployable}
            {@render actions('service-mobile-actions', true)}
          {:else}
            {@render missingVariables('service-mobile-actions', false)}
          {/if}
        {/if}
      </div>
    </div>

    <div use:teleport={'resource-action-hud-slot'} class="hidden w-full items-center xl:flex xl:w-auto">
      <div class="resource-heading-navbar application-heading-actions flex w-auto min-w-0 items-center justify-end gap-1 overflow-visible">
        <div class="resource-heading-actions flex shrink-0 items-center gap-0.5">
          {#if working}{@render deployingIndicator('mr-1')}{/if}
          {#if data.is_deployable}
            <div class="resource-heading-menus shrink-0">{@render linksMenu('desktop', false)}</div>
            {#if data.can_update}{@render actions('service-desktop-actions', false)}{/if}
          {:else if data.can_update}
            {@render missingVariables('service-desktop-actions', true)}
          {/if}
        </div>
      </div>
    </div>
  </div>
</nav>

{#if dialogOpen}
  <ProcessDialog title="Service Startup Logs" closeWithX onclose={() => (dialogOpen = false)}>
    <DatabaseActivityMonitor {activity} />
  </ProcessDialog>
{/if}

{#if confirmRestart}
  <ConfirmModal
    title="Confirm Service Restart?"
    actions={['This service will be restarted.']}
    confirmWithText={false}
    confirmWithPassword={false}
    step2ButtonText="Confirm"
    onconfirm={async () => {
      restart();
    }}
    onclose={() => (confirmRestart = false)}
  />
{/if}

{#if confirmStop}
  <ConfirmModal
    title="Confirm Service Stopping?"
    checkboxes={[{ id: 'docker_cleanup', label: 'Run Docker Cleanup (remove unused images and builder cache).', checked: true }]}
    actions={['This service will be stopped.', 'All non-persistent data will be deleted.']}
    confirmWithText={false}
    confirmWithPassword={false}
    step1ButtonText="Continue"
    step2ButtonText="Confirm"
    onconfirm={async (_password, selectedOptions) => {
      stop(selectedOptions.includes('docker_cleanup'));
    }}
    onclose={() => (confirmStop = false)}
  />
{/if}

{#if confirmRemove}
  <ConfirmModal
    title="Confirm Container Removal?"
    actions={['The exited service resource container will be removed.', 'All non-persistent data will be deleted.']}
    confirmWithText={false}
    confirmWithPassword={false}
    step1ButtonText="Continue"
    step2ButtonText="Confirm"
    onconfirm={async () => {
      removeContainer();
    }}
    onclose={() => (confirmRemove = false)}
  />
{/if}
