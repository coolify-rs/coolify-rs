<script lang="ts">
  // Port of upstream livewire/project/shared/destination.blade.php (+ Shared\Destination).
  import Callout from '../Callout.svelte';
  import ConfirmModal from '../ConfirmModal.svelte';
  import DatabaseStatusSummary from '../DatabaseStatusSummary.svelte';
  import Empty from '../Empty.svelte';
  import Reicon from '../Reicon.svelte';
  import SettingsSection from '../SettingsSection.svelte';
  import StatusBadge from '../StatusBadge.svelte';
  import { ApiError, request } from '../../lib/api';
  import { link, router } from '../../lib/router.svelte';
  import { resPath, type ResourceKind } from '../../lib/sharedApi';
  import { notify } from '../../lib/toast.svelte';

  let {
    kind,
    uuid,
    base = '',
    status: statusProp,
    onchange,
  }: { kind: ResourceKind; uuid: string; base?: string; status?: string; onchange?: () => void } = $props();

  interface Additional {
    network_id: number;
    network: string;
    name: string;
    server_id: number;
    server_uuid: string;
    server_name: string;
    status: string;
  }
  interface Data {
    can_update: boolean;
    can_deploy: boolean;
    is_application: boolean;
    build_pack: string | null;
    status: string | null;
    primary: { network_id: number; network: string; server_id: number; server_uuid: string; server_name: string };
    additional: Additional[];
    has_persistent_storage: boolean;
    networks: { id: number; name: string; server_id: number; server_name: string }[];
    restart_limit_reached: boolean;
    container_present: boolean;
    restart_count: number | null;
    max_restart_count: number | null;
  }

  let data = $state<Data | null>(null);
  let menu = $state<string | null>(null);
  let removing = $state<Additional | null>(null);

  const path = $derived(`${resPath(kind, uuid)}/destination`);
  const status = $derived(statusProp ?? data?.status ?? '');

  async function load() {
    data = await request<Data>('GET', path);
  }

  $effect(() => {
    load();
    const poll = setInterval(() => load().catch(() => {}), 10000);
    return () => clearInterval(poll);
  });

  // stoppedAfterRestartLimit()
  const restartLimit = $derived.by(() => {
    if (!data || !status.startsWith('exited') || !data.container_present || !data.restart_limit_reached) return null;
    return Math.max(data.restart_count ?? 0, data.max_restart_count ?? 0);
  });

  // The match of the single-server card.
  const headline = (s: string) =>
    s
      .replace(/[_-]+/g, ' ')
      .trim()
      .replace(/\b\w/g, (c) => c.toUpperCase());
  const primaryBadge = $derived.by(() => {
    if (status.startsWith('running')) {
      const [state, health] = status.split(':');
      return { label: health ? `${headline(state)} (${health})` : headline(state), type: 'success' as const };
    }
    if (status.startsWith('starting') || status.startsWith('restarting')) {
      const [state, health] = status.split(':');
      return { label: health ? `${headline(state)} (${health})` : headline(state), type: 'warning' as const };
    }
    if (status.startsWith('exited')) return { label: 'Stopped', type: 'neutral' as const };
    return { label: headline(status.split(':')[0]) || 'Unknown', type: 'neutral' as const };
  });

  async function act(action: string, body: Record<string, unknown>) {
    menu = null;
    try {
      const r = await request<{ error?: string; skipped?: string; details?: string; deployment_uuid?: string }>('POST', `${path}/${action}`, body);
      if (r.error) notify.error(r.error, r.details);
      else if (r.skipped) notify.ok(r.skipped, r.details);
      else if (r.deployment_uuid && base) {
        router.go(`${base}/deployment/${r.deployment_uuid}`);
        return;
      }
      await load();
      onchange?.();
    } catch (err) {
      notify.failure(err instanceof ApiError ? err.message : String(err));
    }
  }

  async function remove(password: string): Promise<string | void> {
    if (!removing) return;
    try {
      await request('POST', `${path}/remove`, { password, network_id: removing.network_id, server_id: removing.server_id });
      removing = null;
      await load();
      onchange?.();
    } catch (err) {
      if (err instanceof ApiError && err.message === 'The provided password is incorrect.') return err.message;
      throw err;
    }
  }
</script>

{#snippet actionsButton(id: string)}
  <button type="button" class="button gap-1.5" title="Server actions" aria-expanded={menu === id} aria-haspopup="menu" onclick={() => (menu = menu === id ? null : id)}>
    Actions
    <span class="inline-flex transition-transform {menu === id ? 'rotate-180' : ''}">
      <Reicon name="chevron-down" class="size-3 opacity-55" />
    </span>
  </button>
{/snippet}

{#if data}
  {#if data.is_application}
    <div class="flex flex-col gap-6">
      <SettingsSection id="primary-server-section" title="Primary server" description="The default server and network used for this application's deployments.">
        <div class="flex flex-col gap-4 sm:flex-row sm:items-center sm:justify-between">
          <div class="flex min-w-0 items-center gap-3">
            <div class="flex size-10 shrink-0 items-center justify-center rounded-lg bg-neutral-100 text-neutral-500 ring-1 ring-neutral-200 dark:bg-white/[0.05] dark:text-fg-dim dark:ring-white/[0.07]">
              <Reicon name="servers" class="size-5" />
            </div>
            <div class="min-w-0">
              <div class="flex flex-wrap items-center gap-2">
                <h4 class="truncate text-sm font-semibold text-black dark:text-fg">{data.primary.server_name}</h4>
                <span class="rounded-sm bg-coollabs/10 px-1.5 py-0.5 text-[11px] font-medium text-coollabs dark:bg-brand/15 dark:text-brand-light">Primary</span>
                <DatabaseStatusSummary {status} title="Application status" />
              </div>
              <p class="mt-1 flex flex-wrap items-center gap-1.5 text-[13px] text-neutral-500 dark:text-fg-dim">
                <span>Network</span>
                <code class="rounded bg-neutral-100 px-1.5 py-0.5 font-mono text-xs text-neutral-700 dark:bg-white/[0.05] dark:text-fg-dim">{data.primary.network}</code>
              </p>
            </div>
          </div>

          <div class="flex flex-wrap items-center gap-2 sm:justify-end">
            <a href="/server/{data.primary.server_uuid}" use:link class="button">Open server</a>
            {#if restartLimit !== null}
              <span title="Container has crashed and Coolify stopped it after {restartLimit} restart attempts."><StatusBadge label="Restart limit reached" type="warning" /></span>
            {/if}
            {#if data.additional.length > 0 && data.can_deploy}
              <div class="relative">
                {@render actionsButton('primary')}
                {#if menu === 'primary'}
                  <div class="listbox-panel top-full! right-0! left-auto! z-[90]! mt-1! w-52! min-w-52!" role="menu">
                    <button type="button" class="listbox-option justify-start! gap-2.5!" role="menuitem" onclick={() => act('redeploy', { network_id: data!.primary.network_id, server_id: data!.primary.server_id })}>
                      <Reicon name="refresh" class="size-3.5 opacity-70" />
                      Deploy
                    </button>
                    {#if status.startsWith('running')}
                      <button type="button" class="listbox-option justify-start! gap-2.5!" role="menuitem" onclick={() => act('stop', { server_id: data!.primary.server_id })}>
                        <Reicon name="stop-circle" class="size-3.5 text-error" />
                        Stop
                      </button>
                    {/if}
                  </div>
                {/if}
              </div>
            {/if}
          </div>
        </div>
      </SettingsSection>

      {#if data.additional.length > 0 && data.build_pack !== 'dockercompose'}
        <SettingsSection id="additional-servers-section" title="Additional servers" description="Deploy this application to more servers and choose which destination is primary." flush>
          <div class="divide-y divide-neutral-200 dark:divide-white/[0.07]">
            {#each data.additional as destination (destination.network_id)}
              <div class="flex flex-col gap-4 px-4 py-4 lg:flex-row lg:items-center lg:justify-between">
                <div class="flex min-w-0 items-center gap-3">
                  <div class="flex size-9 shrink-0 items-center justify-center rounded-lg bg-neutral-100 text-neutral-500 ring-1 ring-neutral-200 dark:bg-white/[0.05] dark:text-fg-dim dark:ring-white/[0.07]">
                    <Reicon name="servers" class="size-[18px]" />
                  </div>
                  <div class="min-w-0">
                    <div class="flex flex-wrap items-center gap-2">
                      <h4 class="truncate text-sm font-semibold text-black dark:text-fg">{destination.server_name}</h4>
                      <DatabaseStatusSummary status={destination.status} title="Application status" />
                    </div>
                    <p class="mt-1 flex flex-wrap items-center gap-1.5 text-[13px] text-neutral-500 dark:text-fg-dim">
                      <span>Network</span>
                      <code class="rounded bg-neutral-100 px-1.5 py-0.5 font-mono text-xs text-neutral-700 dark:bg-white/[0.05] dark:text-fg-dim">{destination.network}</code>
                    </p>
                  </div>
                </div>

                <div class="flex flex-wrap items-center gap-2 lg:justify-end">
                  <a href="/server/{destination.server_uuid}" use:link class="button">Open server</a>
                  {#if data.can_deploy}
                    <div class="relative">
                      {@render actionsButton(`additional-${destination.network_id}`)}
                      {#if menu === `additional-${destination.network_id}`}
                        <div class="listbox-panel top-full! right-0! left-auto! z-[90]! mt-1! w-52! min-w-52!" role="menu">
                          <button type="button" class="listbox-option justify-start! gap-2.5!" role="menuitem" onclick={() => act('redeploy', { network_id: destination.network_id, server_id: destination.server_id })}>
                            <Reicon name="refresh" class="size-3.5 opacity-70" />
                            Deploy
                          </button>
                          {#if data.can_update}
                            <button type="button" class="listbox-option justify-start! gap-2.5!" role="menuitem" onclick={() => act('promote', { network_id: destination.network_id, server_id: destination.server_id })}>
                              <Reicon name="shield-star" class="size-3.5 opacity-70" />
                              Make primary
                            </button>
                          {/if}
                          {#if destination.status.startsWith('running')}
                            <button type="button" class="listbox-option justify-start! gap-2.5!" role="menuitem" onclick={() => act('stop', { server_id: destination.server_id })}>
                              <Reicon name="stop-circle" class="size-3.5 text-error" />
                              Stop
                            </button>
                          {/if}
                          {#if data.can_update}
                            <button
                              type="button"
                              class="listbox-option justify-start! gap-2.5!"
                              role="menuitem"
                              onclick={() => {
                                menu = null;
                                removing = destination;
                              }}
                            >
                              <Reicon name="trash" class="size-3.5 text-error" />
                              Remove
                            </button>
                          {/if}
                        </div>
                      {/if}
                    </div>
                  {/if}
                </div>
              </div>
            {/each}
          </div>
        </SettingsSection>
      {/if}

      {#if data.build_pack !== 'dockercompose'}
        <SettingsSection id="available-servers-section" title="Add another server" description="Attach another available server and network as a deployment destination." flush={!data.has_persistent_storage}>
          {#if data.has_persistent_storage}
            <Callout type="warning" title="Additional servers are unavailable">
              This application has persistent storage volumes. Applications with persistent storage
              cannot use multiple servers because those volumes are not shared between hosts.
            </Callout>
          {:else if data.networks.length > 0}
            <div class="divide-y divide-neutral-200 dark:divide-white/[0.07]">
              {#each data.networks as network (network.id)}
                <div class="flex flex-col gap-3 px-4 py-4 sm:flex-row sm:items-center sm:justify-between">
                  <div class="flex min-w-0 items-center gap-3">
                    <div class="flex size-9 shrink-0 items-center justify-center rounded-lg bg-neutral-100 text-neutral-500 ring-1 ring-neutral-200 dark:bg-white/[0.05] dark:text-fg-dim dark:ring-white/[0.07]">
                      <Reicon name="plus" class="size-[18px]" />
                    </div>
                    <div class="min-w-0">
                      <h4 class="truncate text-sm font-semibold text-black dark:text-fg">{network.server_name}</h4>
                      <p class="mt-1 text-[13px] text-neutral-500 dark:text-fg-dim">
                        Network
                        <code class="ml-1 rounded bg-neutral-100 px-1.5 py-0.5 font-mono text-xs text-neutral-700 dark:bg-white/[0.05] dark:text-fg-dim">{network.name}</code>
                      </p>
                    </div>
                  </div>
                  <button type="button" class="button" disabled={!data.can_update} onclick={() => act('add', { network_id: network.id, server_id: network.server_id })}>Add server</button>
                </div>
              {/each}
            </div>
          {:else}
            <Empty size="sm" title="No servers available" description="Every usable server is already attached, or no additional server networks are configured." iconName="servers" />
          {/if}
        </SettingsSection>
      {/if}
    </div>
  {:else}
    <div class="flex flex-col gap-6">
      <SettingsSection id="primary-server-section" title="Primary server" description="The server and network used by this resource.">
        <div class="flex flex-col gap-4 sm:flex-row sm:items-center sm:justify-between">
          <div class="flex min-w-0 items-center gap-3">
            <div class="flex size-10 shrink-0 items-center justify-center rounded-lg bg-neutral-100 text-neutral-500 ring-1 ring-neutral-200 dark:bg-white/[0.05] dark:text-fg-dim dark:ring-white/[0.07]">
              <Reicon name="servers" class="size-5" />
            </div>
            <div class="min-w-0">
              <div class="flex flex-wrap items-center gap-2">
                <h4 class="truncate text-sm font-semibold text-black dark:text-fg">{data.primary.server_name}</h4>
                <span class="rounded-sm bg-coollabs/10 px-1.5 py-0.5 text-[11px] font-medium text-coollabs dark:bg-brand/15 dark:text-brand-light">Primary</span>
              </div>
              <p class="mt-1 flex flex-wrap items-center gap-1.5 text-[13px] text-neutral-500 dark:text-fg-dim">
                <span>Network</span>
                <code class="rounded bg-neutral-100 px-1.5 py-0.5 font-mono text-xs text-neutral-700 dark:bg-white/[0.05] dark:text-fg-dim">{data.primary.network}</code>
              </p>
            </div>
          </div>

          <div class="flex flex-wrap items-center gap-2 sm:justify-end">
            <a href="/server/{data.primary.server_uuid}" use:link class="button">Open server</a>
            <StatusBadge label={primaryBadge.label} type={primaryBadge.type} />
          </div>
        </div>
      </SettingsSection>
    </div>
  {/if}
{/if}

{#if removing}
  <ConfirmModal
    title="Remove server from application?"
    actions={['This will stop the application on this server and remove it as a deployment destination.']}
    confirmWithText
    confirmWithPassword
    confirmationText={removing.server_name}
    confirmationLabel="Enter the server name to confirm removal"
    shortConfirmationLabel="Server name"
    onconfirm={remove}
    onclose={() => (removing = null)}
  />
{/if}
