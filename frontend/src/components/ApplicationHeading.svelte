<script lang="ts">
  // Port of upstream livewire/project/application/heading.blade.php (+ the
  // Heading component): the title, status and links below xl, the Deploy /
  // Redeploy split action (teleported into the top bar on desktop) with the
  // restart and stop confirmations, the "Deploying…" indicator
  // (components/deploying-indicator.blade.php), the links menu
  // (components/applications/links.blade.php) and the configuration-changed
  // warning (livewire/project/shared/configuration-checker.blade.php).
  import Reicon from './Reicon.svelte';
  import Callout from './Callout.svelte';
  import StatusBadge from './StatusBadge.svelte';
  import ConfirmModal from './ConfirmModal.svelte';
  import DatabaseTopbar from './DatabaseTopbar.svelte';
  import DatabaseStatusSummary from './DatabaseStatusSummary.svelte';
  import { appUi, ApiError, type ApplicationRecord, type ApplicationState } from '../lib/api';
  import { teleport } from '../lib/databaseMenu';
  import { router, link } from '../lib/router.svelte';
  import { notify } from '../lib/toast.svelte';

  let {
    record,
    appState,
    base,
    resources,
    onrefresh,
  }: {
    record: ApplicationRecord;
    appState: ApplicationState | null;
    base: string;
    resources: { label: string; href: string; active: boolean }[];
    onrefresh: () => Promise<void>;
  } = $props();

  const status = $derived(record.status ?? 'exited');
  const exited = $derived(status.startsWith('exited'));
  const functional = $derived(record.server_status !== false);
  const canDeploy = $derived(!!appState?.can_deploy);
  // stoppedAfterRestartLimit()
  const restartLimit = $derived.by(() => {
    if (!exited || record.container_present !== true || record.restart_limit_reached !== true) return null;
    return Math.max(Number(record.restart_count ?? 0), Number(record.max_restart_count ?? 0));
  });
  const configurationChanged = $derived(!!appState?.configuration_changed && !exited);
  const hasMobileWarningSlot = typeof document !== 'undefined' && !!document.getElementById('configuration-warning-hud-slot-mobile');
  const healthcheckUrl = $derived(`${base}/healthcheck`);

  // getFqdnWithoutPort()
  function withoutPort(url: string): string {
    try {
      const u = new URL(url.trim());
      u.port = '';
      return u.toString().replace(/\/$/, u.pathname === '/' && !url.trim().endsWith('/') ? '' : '/');
    } catch {
      return url.trim();
    }
  }
  const links = $derived.by(() => {
    const out: { href: string; label: string; badge: string | null }[] = [];
    for (const f of (record.fqdn ?? '').split(',').filter((x) => x.trim() !== '')) {
      out.push({ href: withoutPort(f), label: withoutPort(f), badge: 'Production' });
    }
    for (const m of (record.ports_mappings ?? '').split(',').filter((x) => x.trim() !== '')) {
      const host = m.split(':')[0];
      if (appState?.server_id === 0) out.push({ href: `http://localhost:${host}`, label: `Port ${m}`, badge: null });
      else if (appState?.server_ip) out.push({ href: `http://${appState.server_ip}:${host}`, label: `${appState.server_ip}:${host}`, badge: null });
    }
    return out;
  });

  let busy = $state(false);
  // `needsComposeFile()`.
  const needsComposeFile = $derived(record.build_pack === 'dockercompose' && record.docker_compose_raw == null);

  /** `Heading::loadComposeFile`. */
  async function loadComposeFile(isInit: boolean) {
    busy = true;
    try {
      const r = await appUi.loadCompose(record.uuid, isInit);
      if (r.message) notify.success(r.message);
    } catch (e) {
      notify.failure(e instanceof ApiError ? e.message : String(e));
    } finally {
      busy = false;
    }
    await onrefresh();
  }

  // `x-init="@js($loadComposeOnOpen) && $wire.loadComposeFile(true)"`: on the configuration page.
  let composeLoadTried = false;
  $effect(() => {
    if (composeLoadTried || !needsComposeFile || !appState?.can_update) return;
    composeLoadTried = true;
    if (location.pathname === base) loadComposeFile(true);
  });
  let confirmRestart = $state(false);
  let confirmStop = $state(false);
  let menuOpen = $state<string | null>(null);
  let linksOpen = $state<string | null>(null);
  let warningOpen = $state<string | null>(null);

  const message = (e: unknown) => (e instanceof ApiError ? e.message : String(e));

  async function deploy(force = false) {
    menuOpen = null;
    busy = true;
    try {
      const r = await appUi.deploy(record.uuid, force);
      if (r.deployment_uuid) router.go(`${base}/deployment/${r.deployment_uuid}`);
      else if (r.toast === 'success') notify.ok(r.title ?? 'Success', r.message);
      else notify.error(r.title ?? 'Error', r.message);
    } catch (e) {
      notify.error(message(e));
    } finally {
      busy = false;
    }
  }

  async function restart() {
    try {
      const r = await appUi.restart(record.uuid);
      if (r.deployment_uuid) router.go(`${base}/deployment/${r.deployment_uuid}`);
      else if (r.toast === 'success') notify.ok(r.title ?? 'Success', r.message);
      else notify.error(r.title ?? 'Error', r.message);
    } catch (e) {
      notify.error(message(e));
    }
  }

  async function stop() {
    notify.info('Gracefully stopping application.<br/>It could take a while depending on the application.');
    try {
      await appUi.stop(record.uuid);
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

{#snippet deploying(cls: string)}
  {#if appState?.running_deployment_url}
    <a
      href={appState.running_deployment_url}
      use:link
      class="inline-flex shrink-0 items-center gap-1.5 rounded-md px-2 py-1 text-[11px] font-medium ring-1 transition-colors bg-coollabs/10 text-coollabs ring-coollabs/25 hover:bg-coollabs/15 hover:no-underline dark:bg-brand/15 dark:text-brand-light dark:ring-brand-light/25 dark:hover:bg-brand/20 {cls}"
      title="View the running deployment log"
    >
      <svg class="size-3 animate-spin" viewBox="0 0 24 24" fill="none" aria-hidden="true">
        <circle cx="12" cy="12" r="9" stroke="currentColor" stroke-width="2.5" opacity="0.25" />
        <path d="M21 12a9 9 0 0 0-9-9" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" />
      </svg>
      <span>Deploying…</span>
    </a>
  {/if}
{/snippet}

{#snippet linksMenu(key: string, compact: boolean)}
  <div class="relative" data-links={key}>
    <button
      type="button"
      onclick={() => (linksOpen = linksOpen === key ? null : key)}
      title="Open application links"
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
        class="listbox-panel {compact ? 'w-max! min-w-60! max-w-[min(28rem,calc(100vw-2rem))]!' : 'top-full! mt-1! right-0! left-auto! min-w-60! max-w-96!'}"
      >
        {#each links as l (l.href + l.label)}
          <a class="listbox-option justify-start! gap-2.5!" target="_blank" rel="noreferrer" href={l.href}>
            {#if l.badge}<span class="deployment-badge deployment-badge-production">{l.badge}</span>{/if}
            <span class="min-w-0 truncate">{l.label}</span>
          </a>
        {:else}
          <div class="px-2 py-1.5 text-xs text-neutral-500 dark:text-fg-dim">No links available</div>
        {/each}
      </div>
    {/if}
  </div>
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
  {#if needsComposeFile}
    <div class="flex items-center gap-1">
      {#if appState?.can_update}
        <button type="button" class="button" disabled={busy} onclick={() => loadComposeFile(false)}>Load compose</button>
      {/if}
      {#if canDeploy}
        <button type="button" {id} class="split-action-main rounded-md!" disabled title="Load the Compose file before you deploy.">
          <Reicon name="play-circle" class="size-3.5" />
          Deploy
        </button>
      {/if}
    </div>
  {:else}
  <div class="split-action relative" {id}>
    {#if exited}
      <button type="button" class="split-action-main" disabled={busy} onclick={() => deploy()}>
        {#if busy}{@render spinner()}{:else}<Reicon name="play-circle" class="size-3.5" />{/if}
        <span>{restartLimit !== null ? 'Retry deployment' : 'Deploy'}</span>
      </button>
    {:else}
      <button type="button" class="split-action-main" disabled={busy} onclick={() => deploy()}>
        {#if busy}{@render spinner()}{:else}<Reicon name="refresh" class="size-3.5" />{/if}
        <span>Redeploy</span>
      </button>
    {/if}
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
        {#if exited}
          <button type="button" class="listbox-option justify-start! gap-2.5!" onclick={() => deploy(true)} role="menuitem">
            <Reicon name="play-circle" class="size-3.5" />
            {restartLimit !== null ? 'Retry deployment (without cache)' : 'Deploy (without cache)'}
          </button>
          {#if record.container_present !== false}
            <button type="button" class="listbox-option justify-start! gap-2.5!" onclick={() => ((menuOpen = null), (confirmStop = true))} role="menuitem">
              <Reicon name="stop-circle" class="size-3.5 text-error" />
              Remove container
            </button>
          {/if}
        {:else}
          <button type="button" class="listbox-option justify-start! gap-2.5!" onclick={() => deploy(true)} role="menuitem">
            <Reicon name="refresh" class="size-3.5" />
            Redeploy (without cache)
          </button>
          <button type="button" class="listbox-option justify-start! gap-2.5!" onclick={() => ((menuOpen = null), (confirmRestart = true))} role="menuitem">
            <Reicon name="restart" class="size-3.5" />
            Restart
          </button>
          <button type="button" class="listbox-option justify-start! gap-2.5!" onclick={() => ((menuOpen = null), (confirmStop = true))} role="menuitem">
            <Reicon name="stop-circle" class="size-3.5 text-error" />
            Stop
          </button>
        {/if}
      </div>
    {/if}
  </div>
  {/if}
{/snippet}

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
  <DatabaseTopbar label={record.name} items={resources} {status} {healthcheckUrl} {restartLimit} statusTitle="Application status" />
</div>

{#if configurationChanged}
  <div use:teleport={'configuration-warning-hud-slot'}>{@render warning('desktop')}</div>
  {#if hasMobileWarningSlot}
    <div use:teleport={'configuration-warning-hud-slot-mobile'}>{@render warning('mobile')}</div>
  {/if}
{/if}

<nav class="w-full max-w-none">
  <div>
    {#if record.build_pack === 'nixpacks'}
      <Callout type="warning" title="Nixpacks is deprecated" class="mb-4">
        Nixpacks is deprecated and is in maintenance mode upstream. We recommend Railpack, the successor of Nixpacks, for new and existing applications.
        <a class="underline" href={base} use:link>Switch to Railpack in General settings.</a>
      </Callout>
    {/if}
    <div class="mb-3 flex w-full min-w-0 items-start gap-3 xl:hidden">
      <div class="min-w-0 flex-1">
        <div class="flex min-w-0 flex-col items-start gap-2">
          <h1 class="min-w-0 max-w-full truncate text-[24px]! leading-7! font-semibold! tracking-tight! text-black dark:text-fg">{record.name}</h1>
          <div class="relative flex w-full min-w-0 items-center gap-2">
            <DatabaseStatusSummary {status} {healthcheckUrl} title="Application status" />
            {@render linksMenu('mobile', true)}
            {@render deploying('')}
            {#if restartLimit !== null}
              <span title="Container has crashed and coolify-rs stopped it after {restartLimit} restart attempts."
                ><StatusBadge label="Restart limit reached" type="warning" /></span
              >
            {/if}
          </div>
          <div class="flex w-full flex-wrap gap-1"></div>
        </div>
      </div>
      <div class="shrink-0">
        {#if functional && canDeploy}{@render actions('application-mobile-actions')}{/if}
      </div>
    </div>

    <div use:teleport={'resource-action-hud-slot'} class="hidden w-full items-center xl:flex xl:w-auto">
      <div class="resource-heading-navbar application-heading-actions flex w-auto min-w-0 items-center justify-end gap-1 overflow-visible">
        <div class="resource-heading-actions flex shrink-0 items-center gap-0.5">
          {@render deploying('mr-1')}
          {#if !needsComposeFile}{@render linksMenu('desktop', false)}{/if}
          {#if functional}
            {#if canDeploy}{@render actions('application-desktop-actions')}{/if}
          {:else}
            <StatusBadge label="Server unavailable" type="error" />
          {/if}
        </div>
      </div>
    </div>
  </div>
</nav>

{#if confirmRestart}
  <ConfirmModal
    title="Confirm Application Restart?"
    actions={['This application will be restarted without rebuilding.']}
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
    title={exited ? 'Confirm Container Removal?' : 'Confirm Application Stopping?'}
    actions={exited
      ? ['The exited application container will be removed.', 'Anonymous volumes may become eligible for Docker cleanup.']
      : ['This application will be stopped.', 'All non-persistent data of this application will be deleted.']}
    checkboxes={[{ id: 'docker_cleanup', label: 'Run Docker Cleanup (remove unused images and builder cache).', checked: true }]}
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

