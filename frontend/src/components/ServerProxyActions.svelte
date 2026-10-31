<script lang="ts">
  // Port of the proxy actions of upstream livewire/server/navbar.blade.php
  // (`x-split-action` in the top bar): start, restart, stop and refresh status.
  // The "Proxy Startup Logs" process dialog is not ported; status follows by polling.
  import ConfirmModal from './ConfirmModal.svelte';
  import Reicon from './Reicon.svelte';
  import { portal } from '../lib/portal';
  import { post, failure } from '../lib/serverApi';
  import type { ServerTopbar } from '../lib/page.svelte';
  import { session } from '../lib/session.svelte';
  import { router } from '../lib/router.svelte';
  import { notify } from '../lib/toast.svelte';

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

  async function act(action: 'start' | 'restart' | 'stop' | 'status') {
    open = false;
    try {
      await post(`/server-page/${encodeURIComponent(server.uuid)}/proxy/action/${action}`);
      if (action === 'status') notify.success('Proxy status updated.');
      else window.setTimeout(() => router.go(router.path + router.search, true), 1500);
    } catch (e) {
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
