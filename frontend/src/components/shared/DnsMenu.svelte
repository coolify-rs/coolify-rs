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
  async function manualRecords() {
    open = false;
    await load();
    if ((data?.proposals.length ?? 0) > 0) {
      showProviders = true;
    } else {
      showRecords = true;
    }
  }
  async function createRecord(p: Proposal) {
    try {
      const r = await request<{ message?: string; conflict?: Conflict }>('POST', `${base()}/create`, { hostname: p.hostname, zone_id: p.zone_id });
      if (r.conflict) {
        conflicts[`${p.hostname}|${p.zone_id}`] = r.conflict;
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
  function copyRecords() {
    if (!data) return;
    const text = data.hints.map((h) => `${h.name} ${h.type} ${h.value}`).join('\n') + '\n';
    navigator.clipboard?.writeText(text);
    notify.success('DNS records copied.');
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
  <Modal title="DNS records" onclose={() => (showRecords = false)}>
    {#if data.hints.length === 0}
      <p class="text-sm text-neutral-600 dark:text-fg-dim">No DNS records are needed right now.</p>
    {:else}
      <table class="w-full text-left text-[13px]">
        <thead><tr><th>Type</th><th>Name</th><th>Value</th></tr></thead>
        <tbody>
          {#each data.hints as h (`${h.type}|${h.name}|${h.value}`)}
            <tr><td>{h.type}</td><td class="font-mono">{h.name}</td><td class="font-mono">{h.value}</td></tr>
          {/each}
        </tbody>
      </table>
      <div class="mt-3 flex justify-end gap-2">
        <button type="button" class="button" onclick={copyRecords}>Copy records</button>
        <button type="button" class="button" onclick={checkAll}>Check again</button>
      </div>
    {/if}
  </Modal>
{/if}

{#if showProviders && data}
  <Modal title="Configure DNS" onclose={() => (showProviders = false)}>
    <div class="flex flex-col gap-3">
      <p class="text-sm text-neutral-600 dark:text-fg-dim">Coolify matched each hostname to a zone available through your connected DNS providers.</p>
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
          {#if p.managed}
            <span class="text-xs text-green-600">Managed by Coolify</span>
          {:else if conflict}
            <div class="flex flex-col items-end gap-2 text-xs">
              <span class="text-red-600 dark:text-red-400">Currently {conflict.current}</span>
              <button type="button" class="button button-danger" onclick={() => (replacing = p)}>Replace record</button>
            </div>
          {:else if !p.target}
            <span class="text-xs text-neutral-500 dark:text-fg-dim">Add manually</span>
          {:else if data.can_update}
            <button type="button" class="button" {...{ ishighlighted: '' }} onclick={() => createRecord(p)}>Create DNS record</button>
          {/if}
        </div>
      {/each}
      <div class="flex justify-end">
        <button type="button" class="button" onclick={() => { showProviders = false; showRecords = true; }}>Manual records</button>
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
