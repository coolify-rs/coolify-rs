<script lang="ts">
  // Port of the proxy actions of upstream livewire/server/navbar.blade.php
  // (`x-split-action` in the top bar): start, restart, stop and refresh status, with
  // the "Proxy Startup Logs" process dialog. Status follows by polling.
  import { onDestroy } from 'svelte';
  import ConfirmModal from './ConfirmModal.svelte';
  import DatabaseActivityMonitor from './DatabaseActivityMonitor.svelte';
  import ProcessDialog from './ProcessDialog.svelte';
  import Reicon from './Reicon.svelte';
  import { portal } from '../lib/portal';
  import { get, post, failure } from '../lib/serverApi';
  import type { DatabaseState } from '../lib/api';
  import type { ServerTopbar } from '../lib/page.svelte';
  import { session } from '../lib/session.svelte';
  import { router } from '../lib/router.svelte';

  let { server }: { server: ServerTopbar } = $props();
  let open = $state(false);
  let confirm = $state<'restart' | 'stop' | null>(null);
  let root: HTMLDivElement | undefined = $state();
  const canStop = $derived(['running', 'starting', 'restarting'].includes(server.proxyStatus ?? ''));

  function teleport(node: HTMLElement, id: string) {
    document.getElementById(id)?.appendChild(node);
    return {
      destroy() {
        node.remove();
      },
    };
  }

  let dialogOpen = $state(false);
  let activity = $state<DatabaseState['activity']>(null);
  let poll: ReturnType<typeof setInterval> | undefined;
  onDestroy(() => clearInterval(poll));

  /** `$wire.$on('activityMonitor')`: the activity's log until it ends, then the new status in the top bar. */
  function follow(after: number) {
    clearInterval(poll);
    poll = setInterval(async () => {
      try {
        const r = await get<{ activity: DatabaseState['activity'] }>(`/server-page/${encodeURIComponent(server.uuid)}/proxy/activity?after=${after}`);
        activity = r.activity;
        if (r.activity && (r.activity.status === 'finished' || r.activity.status === 'error')) {
          clearInterval(poll);
          router.go(router.path + router.search, true);
        }
      } catch {
        // The next tick tries again: a restart can interrupt the connection.
      }
    }, 1000);
  }

  async function act(action: 'start' | 'restart' | 'stop' | 'status') {
    open = false;
    // `window.dispatchEvent(new CustomEvent('startproxy'))` opens the dialog at once.
    if (action === 'start' || action === 'restart') {
      activity = null;
      dialogOpen = true;
    }
    try {
      const r = await post<{ activity_after: number }>(`/server-page/${encodeURIComponent(server.uuid)}/proxy/action/${action}`);
      if (action === 'start' || action === 'restart') follow(r.activity_after);
      else window.setTimeout(() => router.go(router.path + router.search, true), action === 'stop' ? 1500 : 0);
    } catch (e) {
      dialogOpen = false;
      failure(e);
    }
  }
  void portal;
</script>

<svelte:window onclick={(e) => open && root && !root.contains(e.target as Node) && (open = false)} onkeydown={(e) => e.key === 'Escape' && (open = false)} />

{#if session.isAdmin}
  <div use:teleport={'resource-action-hud-slot'} class="hidden xl:flex xl:w-auto xl:items-center">
    <div class="resource-heading-navbar application-heading-actions flex w-auto min-w-0 items-center justify-end gap-1 overflow-visible">
      <div class="split-action relative resource-heading-actions shrink-0" bind:this={root}>
        <button type="button" class="split-action-main" onclick={() => (canStop ? (confirm = 'restart') : act('start'))}>
          <Reicon name={canStop ? 'restart' : 'play-circle'} class="size-3.5" />
          {canStop ? 'Restart Proxy' : 'Start Proxy'}
        </button>
        <button type="button" class="split-action-caret" onclick={() => (open = !open)} aria-expanded={open} aria-haspopup="menu" aria-label="More actions">
          <span class="inline-flex transition-transform {open ? 'rotate-180' : ''}"><Reicon name="chevron-down" class="size-3" /></span>
        </button>
        {#if open}
          <div class="listbox-panel right-0! left-auto! xl:min-w-60! xl:max-w-96!" role="menu">
            {#if canStop}
              <button type="button" class="listbox-option justify-start! gap-2.5!" role="menuitem" onclick={() => ((open = false), (confirm = 'stop'))}>
                <Reicon name="stop-circle" class="size-3.5 text-error" />
                Stop Proxy
              </button>
            {/if}
            <button type="button" class="listbox-option justify-start! gap-2.5!" role="menuitem" onclick={() => act('status')}>
              <Reicon name="refresh" class="size-3.5 opacity-70" />
              Refresh Proxy Status
            </button>
          </div>
        {/if}
      </div>
    </div>
  </div>
{/if}

{#if dialogOpen}
  <ProcessDialog title="Proxy Startup Logs" closeWithX onclose={() => (dialogOpen = false)}>
    <div class="flex h-full min-h-0 flex-col gap-3">
      {#if server.isCoolifyHost}
        <div class="shrink-0 rounded-lg border border-warning/30 bg-warning/10 p-3 text-sm text-warning">
          <span class="font-semibold">Note:</span> This is the localhost server where Coolify runs. During proxy restart, the connection may be temporarily lost. If logs stop updating, please refresh the browser after a few minutes.
        </div>
      {/if}
      <div class="flex min-h-0 flex-1 flex-col">
        <DatabaseActivityMonitor {activity} />
      </div>
    </div>
  </ProcessDialog>
{/if}

{#if confirm === 'restart'}
  <ConfirmModal
    title="Confirm Proxy Restart?"
    actions={['This proxy will be stopped and started again.', 'All resources hosted on this server will be temporarily unavailable.']}
    confirmWithText={false}
    step2ButtonText="Restart Proxy"
    onconfirm={() => act('restart')}
    onclose={() => (confirm = null)}
  />
{:else if confirm === 'stop'}
  <ConfirmModal
    title="Confirm Proxy Stopping?"
    actions={['The Coolify proxy will be stopped.', 'All resources hosted on this server will be inaccessible.']}
    confirmWithText={false}
    step2ButtonText="Stop Proxy"
    onconfirm={() => act('stop')}
    onclose={() => (confirm = null)}
  />
{/if}
