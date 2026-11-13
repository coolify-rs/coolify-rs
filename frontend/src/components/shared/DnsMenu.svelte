<script lang="ts">
  // Port of upstream livewire/project/shared/cloudflare-autoconfigure.blade.php (the DNS menu and the
  // manual records) and dns-provider-management.blade.php. Domain Connect needs Coolify Cloud.
  import Callout from '../Callout.svelte';
  import ConfirmModal from '../ConfirmModal.svelte';
  import Modal from '../Modal.svelte';
  import Reicon from '../Reicon.svelte';
  import { ApiError, request } from '../../lib/api';
  import { resPath, type ResourceKind } from '../../lib/sharedApi';
  import { notify } from '../../lib/toast.svelte';

  let { kind, uuid, showCheckAll = true, onchange }: { kind: ResourceKind; uuid: string; showCheckAll?: boolean; onchange?: () => void } = $props();

  interface Proposal { hostname: string; zone_id: number; zone: string; credential: string; target: string | null; managed: boolean }
  interface Conflict { record_id: string; current: string; proposed: string }
  interface Data {
    can_update: boolean;
    hints: { type: string; name: string; value: string }[];
    no_public_address_message: string;
    server_ip: string | null;
    proposals: Proposal[];
  }

  const base = () => `${resPath(kind, uuid)}/dns`;
  let open = $state(false);
  let data = $state<Data | null>(null);
  let showRecords = $state(false);
  let showProviders = $state(false);
  let conflicts = $state<Record<string, Conflict>>({});
  let replacing = $state<Proposal | null>(null);

  const load = async () => (data = await request<Data>('GET', base()));
  const fail = (e: unknown) => notify.failure(e instanceof ApiError ? e.message : String(e));

  async function checkAll() {
    open = false;
    try {
      await request('POST', `${base()}/check`, {});
      onchange?.();
    } catch (e) {
      fail(e);
    }
  }
  /** `$showDnsProviderModal = true` of `configureDnsAfterDomainAdd`. */
  export async function openProviders() {
    await load();
    showProviders = true;
  }
  /**
   * `dnsRecordConfigurationFinished` with a conflict: the record that blocked the automatic create
   * shows in the provider modal, where the user can replace it.
   */
  export async function showConflict(hostname: string, zoneId: number, conflict: Conflict) {
    conflicts[`${hostname.toLowerCase()}|${zoneId}`] = conflict;
    await load();
    showRecords = false;
    showProviders = true;
  }
  /** `openManualDnsRecords`. */
  async function manualRecords() {
    open = false;
    await load();
    showRecords = true;
  }
  async function createRecord(p: Proposal) {
    try {
      const r = await request<{ message?: string; conflict?: Conflict }>('POST', `${base()}/create`, { hostname: p.hostname, zone_id: p.zone_id });
      if (r.conflict) {
        conflicts[`${p.hostname}|${p.zone_id}`] = r.conflict;
        showRecords = false;
        showProviders = true;
      } else if (r.message) {
        notify.success(r.message);
        await load();
        onchange?.();
      }
    } catch (e) {
      fail(e);
    }
  }
  async function replaceRecord(p: Proposal) {
    const key = `${p.hostname}|${p.zone_id}`;
    const c = conflicts[key];
    replacing = null;
    try {
      const r = await request<{ message: string }>('POST', `${base()}/replace`, { hostname: p.hostname, zone_id: p.zone_id, ...c });
      notify.success(r.message);
      await load();
      onchange?.();
    } catch (e) {
      fail(e);
    } finally {
      delete conflicts[key];
    }
  }
  let copied = $state(false);
  const copy = (text: string) => navigator.clipboard?.writeText(text);
  /** `DnsRecordHints::toCopyText`. */
  function copyRecords() {
    if (!data) return;
    const names = data.hints.map((h) => (h.name === '' || h.name === '@' ? '@' : `${h.name.replace(/\.+$/, '')}.`));
    const width = Math.max(...names.map((n) => n.length));
    const lines = data.hints.map((h, i) => `${names[i].padEnd(width)}  IN ${h.type.toUpperCase().padEnd(5)} ${h.value}`);
    copy(`${lines.join('\n')}\n`);
    copied = true;
    setTimeout(() => (copied = false), 2000);
  }
  /** `recheckDnsRecordsInModal`. */
  async function recheck() {
    try {
      await request('POST', `${base()}/check`, {});
      onchange?.();
      await load();
    } catch (e) {
      fail(e);
    }
  }
</script>

<div class="relative">
  <button type="button" class="button" aria-haspopup="menu" aria-expanded={open} title="DNS checks and records for this server" onclick={() => (open = !open)}>
    <Reicon name="globe" class="size-3.5" />
    DNS
    <span class="inline-flex transition-transform" class:rotate-180={open}>
      <svg xmlns="http://www.w3.org/2000/svg" fill="none" viewBox="0 0 24 24" stroke-width="2" stroke="currentColor" class="size-3.5 shrink-0 opacity-60">
        <path stroke-linecap="round" stroke-linejoin="round" d="m8 9 4-4 4 4m0 6-4 4-4-4" />
      </svg>
    </span>
  </button>
  {#if open}
    <div role="menu" class="listbox-panel left-auto! right-0! z-[90]! w-56! min-w-56!">
      {#if showCheckAll}
        <button type="button" class="listbox-option justify-start! gap-2.5!" role="menuitem" onclick={checkAll}>
          <Reicon name="refresh" class="size-3.5 shrink-0 opacity-70" />
          Check all DNS
        </button>
      {/if}
      <button type="button" class="listbox-option justify-start! gap-2.5!" role="menuitem" onclick={manualRecords}>
        <Reicon name="documentation" class="size-3.5 shrink-0 opacity-70" />
        Manual records
      </button>
    </div>
  {/if}
</div>

{#if showRecords && data}
  {@const hints = data.hints}
  <div class="fixed inset-0 z-99 overflow-y-auto">
    <button type="button" aria-label="Close" class="absolute inset-0 h-full w-full bg-black/50 backdrop-blur-[2px]" onclick={() => (showRecords = false)}></button>
    <div class="relative flex min-h-full items-start justify-center p-4 sm:items-center">
      <div class="application-settings-form application-settings-section relative flex w-full max-w-2xl flex-col overflow-hidden" style="box-shadow: 0 0 0 1px var(--coollabs-hairline), var(--shadow-modal)">
        <header class="flex-nowrap!">
          <h3 class="min-w-0 flex-1 truncate">DNS entries</h3>
          <button type="button" class="icon-button shrink-0" aria-label="Close" onclick={() => (showRecords = false)}>
            <Reicon name="x" class="size-4" />
          </button>
        </header>
        <div class="application-settings-section-body flex flex-col gap-4">
          <p class="text-sm leading-6 text-neutral-600 dark:text-fg-dim">
            Hosts that still need DNS at your provider (working domains are omitted). Create matching Type / Name / Value records so traffic reaches this server.
          </p>
          {#if !data.server_ip && hints.length === 0}
            <Callout type="warning" title="No server IP">Could not determine a public IP for this destination. Set the server IP (or instance public IPv4 for localhost) first.</Callout>
          {:else if hints.length === 0}
            <Callout type="info" title="Nothing to configure">No pending DNS entries. All listed domains already resolve correctly, or no domains are configured yet. Use Recheck after changing DNS.</Callout>
          {:else}
            {#if data.proposals.length > 0 && data.proposals.some((p) => !p.target)}
              <Callout type="warning" title="No public IP">{data.no_public_address_message}</Callout>
            {/if}
            <div class="overflow-x-auto rounded-md border border-neutral-200 dark:border-coolgray-300">
              <table class="w-full min-w-[32rem] text-left text-sm">
                <thead class="bg-neutral-50 text-[12px] uppercase tracking-wide text-neutral-500 dark:bg-coolgray-100 dark:text-fg-dim">
                  <tr>
                    <th class="px-3 py-2 font-medium">Type</th>
                    <th class="px-3 py-2 font-medium">Name</th>
                    <th class="px-3 py-2 font-medium">Value</th>
                    <th class="px-3 py-2 font-medium"><span class="sr-only">Action</span></th>
                  </tr>
                </thead>
                <tbody class="divide-y divide-neutral-200 dark:divide-coolgray-300">
                  {#each hints as h (`${h.type}|${h.name}|${h.value}`)}
                    <tr class="text-[13px] text-black dark:text-fg">
                      <td class="px-3 py-2.5">{h.type}</td>
                      <td class="px-3 py-2.5">
                        <div class="flex min-w-0 items-center gap-1.5">
                          <span class="min-w-0 break-all">{h.name}</span>
                          <button type="button" class="icon-button shrink-0" aria-label="Copy name" onclick={() => copy(h.name)}><Reicon name="copy" class="size-3.5" /></button>
                        </div>
                      </td>
                      <td class="px-3 py-2.5">
                        <div class="flex min-w-0 items-center gap-1.5">
                          <span class="min-w-0 break-all">{h.value}</span>
                          <button type="button" class="icon-button shrink-0" aria-label="Copy value" onclick={() => copy(h.value)}><Reicon name="copy" class="size-3.5" /></button>
                        </div>
                      </td>
                      <td class="px-3 py-2.5 text-right">
                        {#if data.can_update}
                          {#each data.proposals.filter((p) => p.hostname === h.name && p.target && !p.managed) as p (p.zone_id)}
                            <button type="button" class="button" onclick={() => createRecord(p)}>Add with {p.credential}</button>
                          {/each}
                        {/if}
                      </td>
                    </tr>
                  {/each}
                </tbody>
              </table>
            </div>
            <div class="flex flex-wrap items-center justify-between gap-2">
              <p class="text-[12px] text-neutral-500 dark:text-fg-dim">{hints.length} {hints.length === 1 ? 'entry' : 'entries'} · BIND zone format</p>
              <button type="button" class="button shrink-0" title="Copy as BIND-compatible zone file" onclick={copyRecords}>{copied ? 'Copied' : 'Copy all'}</button>
            </div>
          {/if}
          <div class="flex flex-wrap items-center justify-between gap-2 pt-2">
            <button type="button" class="button" title="Recheck DNS" onclick={recheck}>
              <Reicon name="refresh" class="size-3.5" />
              Recheck
            </button>
            <button type="button" class="button" {...{ ishighlighted: '' }} onclick={() => (showRecords = false)}>Done</button>
          </div>
        </div>
      </div>
    </div>
  </div>
{/if}

{#if showProviders && data}
  <Modal title="Configure DNS" onclose={() => (showProviders = false)}>
    <div class="flex flex-col gap-3">
      <p class="text-sm text-neutral-600 dark:text-fg-dim">coolify-rs matched each hostname to a zone available through your connected DNS providers.</p>
      {#if data.proposals.some((p) => !p.target)}
        <Callout type="warning" title="No public IP">{data.no_public_address_message}</Callout>
      {/if}
      {#each data.proposals as p (`${p.hostname}|${p.zone_id}`)}
        {@const conflict = conflicts[`${p.hostname}|${p.zone_id}`]}
        <div class="flex flex-col gap-3 rounded-lg border border-neutral-200 p-3 dark:border-white/[0.08] sm:flex-row sm:items-center sm:justify-between">
          <div class="min-w-0 text-sm">
            <div class="truncate font-medium text-black dark:text-fg">{p.hostname} → {p.target}</div>
            <div class="text-xs text-neutral-500 dark:text-fg-dim">{p.credential} · {p.zone}</div>
          </div>
          {#if conflict}
            <div class="flex flex-col items-end gap-2 text-xs">
              <span class="text-red-600 dark:text-red-400">Currently {conflict.current}</span>
              <button type="button" class="button button-error" onclick={() => (replacing = p)}>Replace record</button>
            </div>
          {:else if p.managed}
            <span class="text-xs text-green-600">Managed by coolify-rs</span>
          {:else if !p.target}
            <span class="text-xs text-neutral-500 dark:text-fg-dim">Add manually</span>
          {:else if data.can_update}
            <button type="button" class="button" {...{ ishighlighted: '' }} onclick={() => createRecord(p)}>Create DNS record</button>
          {/if}
        </div>
      {/each}
      <div class="flex justify-end">
        <button type="button" class="button" {...{ ishighlighted: '' }} onclick={() => (showProviders = false)}>Done</button>
      </div>
    </div>
  </Modal>
{/if}

{#if replacing}
  {@const c = conflicts[`${replacing.hostname}|${replacing.zone_id}`]}
  <ConfirmModal
    title="Replace conflicting DNS record?"
    actions={[`Replace ${replacing.hostname} value ${c?.current} with ${c?.proposed}.`]}
    confirmWithText={false}
    confirmWithPassword={false}
    step2ButtonText="Replace record"
    onconfirm={async () => { if (replacing) await replaceRecord(replacing); }}
    onclose={() => (replacing = null)}
  />
{/if}
