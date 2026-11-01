<script lang="ts">
  // Port of upstream livewire/project/database/heading.blade.php (+ the
  // Heading component): the title and status below xl, the Start / Restart /
  // Stop split action (teleported into the top bar on desktop), the restart and
  // stop confirmations, the "Database Startup Logs" process dialog with the
  // activity monitor, and the configuration-changed warning of
  // livewire/project/shared/configuration-checker.blade.php
  // (components/configuration-warning.blade.php).
  import { untrack } from 'svelte';
  import Reicon from './Reicon.svelte';
  import StatusBadge from './StatusBadge.svelte';
  import ConfirmModal from './ConfirmModal.svelte';
  import ProcessDialog from './ProcessDialog.svelte';
  import DatabaseTopbar from './DatabaseTopbar.svelte';
  import DatabaseStatusSummary from './DatabaseStatusSummary.svelte';
  import DatabaseActivityMonitor from './DatabaseActivityMonitor.svelte';
  import { dbUi, ApiError, type DatabaseRecord, type DatabaseState } from '../lib/api';
  import { teleport } from '../lib/databaseMenu';
  import { notify } from '../lib/toast.svelte';

  let {
    record,
    dbState,
    healthcheckUrl,
    resources,
    onrefresh,
  }: {
    record: DatabaseRecord;
    dbState: DatabaseState | null;
    healthcheckUrl: string;
    resources: { label: string; href: string; active: boolean }[];
    onrefresh: () => Promise<void>;
  } = $props();

  type Activity = DatabaseState['activity'];

  const canManage = $derived(!!dbState?.can_update);
  const exited = $derived((record.status ?? 'exited').startsWith('exited'));
  const functional = $derived(record.server_status !== false);
  const restartLimit = $derived.by(() => {
    if (!exited || record.restart_limit_reached !== true) return null;
    return Math.max(Number(record.restart_count ?? 0), Number(record.max_restart_count ?? 0));
  });
  // `$isConfigurationChanged && !is_null($resource->config_hash) && !$resource->isExited()`
  const configurationChanged = $derived(!!dbState?.configuration_changed && !exited);
  const hasMobileWarningSlot = typeof document !== 'undefined' && !!document.getElementById('configuration-warning-hud-slot-mobile');

  let busy = $state(false);
  let working = $state(false);
  let dialogOpen = $state(false);
  let activity = $state<Activity>(null);
  let confirmRestart = $state(false);
  let confirmStop = $state(false);
  let menuOpen = $state<string | null>(null);
  let warningOpen = $state<string | null>(null);

  let timer: ReturnType<typeof setTimeout> | undefined;
  let lastActivityId: number | null = null;
  // After a start, the activity of the previous run may still be the latest one for a moment.
  let awaitingNew = false;
  let waits = 0;

  const isRunning = (a: Activity) => !!a && (a.status === 'queued' || a.status === 'in_progress');

  // Heading::mount → checkDeployments
  $effect(() => {
    untrack(() => {
      const a = dbState?.activity ?? null;
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
    let s: DatabaseState;
    try {
      s = await dbUi.state(record.uuid);
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
    await onrefresh();
  }

  /** The start (or restart) was queued: open the log and follow it. */
  function follow() {
    awaitingNew = true;
    waits = 0;
    activity = null;
    working = true;
    dialogOpen = true;
    watch();
  }

  /** `reopenDeployment`: an operation is already running. */
  function reopen() {
    if (working) dialogOpen = true;
    else notify.info('No operation is currently running.');
  }

  const message = (e: unknown) => (e instanceof ApiError ? e.message : String(e));

  async function start() {
    if (working) return reopen();
    busy = true;
    try {
      await dbUi.start(record.uuid);
      follow();
    } catch (e) {
      notify.error(message(e));
    } finally {
      busy = false;
    }
  }

  async function restart() {
    if (working) return reopen();
    busy = true;
    try {
      await dbUi.restart(record.uuid);
      notify.info('Restarting database.');
      follow();
    } catch (e) {
      notify.error(message(e));
    } finally {
      busy = false;
    }
  }

  async function stop() {
    try {
      notify.info('Gracefully stopping database.');
      await dbUi.stop(record.uuid);
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
    if (warningOpen && !t.closest(`[data-warning="${warningOpen}"]`)) warningOpen = null;
  }}
  onkeydown={(e) => e.key === 'Escape' && ((menuOpen = null), (warningOpen = null))}
/>

{#snippet working_indicator(cls: string)}
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
    <span>Working…</span>
  </button>
{/snippet}

{#snippet spinner()}
  <span class="inline-flex shrink-0 items-center gap-1.5">
    <svg class="size-3.5 shrink-0 animate-spin" viewBox="0 0 24 24" fill="none" aria-hidden="true">
      <circle class="opacity-25" cx="12" cy="12" r="9" stroke="currentColor" stroke-width="3" />
      <path class="opacity-75" d="M21 12a9 9 0 0 0-9-9" stroke="currentColor" stroke-width="3" stroke-linecap="round" />
    </svg>
  </span>
{/snippet}

<!-- components/split-action.blade.php -->
{#snippet actions(id: string)}
  <div class="split-action relative" {id}>
    {#if !exited}
      <button type="button" class="split-action-main" disabled={busy} onclick={() => (confirmRestart = true)}>
        {#if busy}{@render spinner()}{:else}<Reicon name="restart" class="size-3.5" />{/if}
        <span>{busy ? 'Restarting…' : 'Restart'}</span>
      </button>
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
      {#if menuOpen === id}
        <div class="listbox-panel right-0! left-auto! xl:min-w-60! xl:max-w-96!" role="menu">
          <button
            type="button"
            class="listbox-option justify-start! gap-2.5!"
            onclick={() => ((menuOpen = null), (confirmStop = true))}
            role="menuitem"
          >
            <Reicon name="stop-circle" class="size-3.5 text-error" />
            Stop
          </button>
        </div>
      {/if}
    {:else}
      <button type="button" class="split-action-main" disabled={busy} onclick={start}>
        {#if busy}{@render spinner()}{:else}<Reicon name="play-circle" class="size-3.5" />{/if}
        <span>{busy ? 'Starting…' : 'Start'}</span>
      </button>
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
  <DatabaseTopbar label={record.name} items={resources} status={record.status} {healthcheckUrl} {restartLimit} />
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
            {record.name}
          </h1>
          <div class="relative flex w-full min-w-0 items-center gap-2">
            <DatabaseStatusSummary status={record.status} {healthcheckUrl} />
            {#if working}{@render working_indicator('')}{/if}
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
        {#if functional && canManage}{@render actions('database-mobile-actions')}{/if}
      </div>
    </div>

    <div use:teleport={'resource-action-hud-slot'} class="hidden w-full items-center xl:flex xl:w-auto">
        <div class="resource-heading-navbar application-heading-actions flex w-auto min-w-0 items-center justify-end gap-1 overflow-visible">
          <div class="resource-heading-actions flex shrink-0 items-center gap-0.5">
            {#if working}{@render working_indicator('mr-1')}{/if}
            {#if functional}
              {#if canManage}{@render actions('database-desktop-actions')}{/if}
            {:else}
              <StatusBadge label="Server unavailable" type="error" />
            {/if}
          </div>
        </div>
    </div>
  </div>
</nav>

{#if dialogOpen}
  <ProcessDialog title="Database Startup Logs" closeWithX onclose={() => (dialogOpen = false)}>
    <DatabaseActivityMonitor {activity} />
  </ProcessDialog>
{/if}

{#if confirmRestart}
  <ConfirmModal
    title="Confirm Database Restart?"
    actions={['This database will be unavailable during the restart.', 'If the database is currently in use, data could be lost.']}
    confirmWithText={false}
    confirmWithPassword={false}
    step2ButtonText="Restart Database"
    onconfirm={async () => {
      restart();
    }}
    onclose={() => (confirmRestart = false)}
  />
{/if}

{#if confirmStop}
  <ConfirmModal
    title="Confirm Database Stopping?"
    actions={[
      'This database will be stopped.',
      'If the database is currently in use, data could be lost.',
      'Non-persistent containers, networks, and unused images will be removed.',
    ]}
    confirmWithText={false}
    confirmWithPassword={false}
    step1ButtonText="Continue"
    step2ButtonText="Confirm"
    onconfirm={async () => {
      stop();
    }}
    onclose={() => (confirmStop = false)}
  />
{/if}
