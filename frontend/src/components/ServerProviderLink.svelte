<script lang="ts">
  // The "Link provider" menu of upstream livewire/server/show.blade.php and
  // components/server/provider-link-modal.blade.php.
  import Callout from './Callout.svelte';
  import FormInput from './FormInput.svelte';
  import Listbox from './Listbox.svelte';
  import Modal from './Modal.svelte';
  import Reicon from './Reicon.svelte';
  import StatusBadge from './StatusBadge.svelte';
  import { get, post, failure } from '../lib/serverApi';
  import { notify } from '../lib/toast.svelte';

  type LinkOption = { provider: string; label: string; tokens: { value: string; label: string }[] };
  type Matched = { name: string; id: string | number | null; status: string | null };
  type SearchResult = { error?: string; no_match?: boolean; matched?: Matched };

  let { uuid, onlinked }: { uuid: string; onlinked: () => void } = $props();

  const MANUAL: Record<string, { label: string; placeholder: string }> = {
    hetzner: { label: 'Server ID', placeholder: '12345678' },
    digitalocean: { label: 'Droplet ID', placeholder: '12345678' },
    vultr: { label: 'Instance ID', placeholder: '6d4b…' },
    hostinger: { label: 'Virtual Machine ID', placeholder: '17923' },
  };

  let providers = $state<LinkOption[]>([]);
  let open = $state(false);
  let active = $state<LinkOption | null>(null);
  let tokenId = $state<string | undefined>(undefined);
  let manualId = $state('');
  let result = $state<SearchResult>({});
  let searching = $state<'' | 'id' | 'ip'>('');
  let menu = $state<HTMLElement>();

  async function load() {
    try {
      providers = (await get<{ providers: LinkOption[] }>(`/server-page/${encodeURIComponent(uuid)}/provider-links`)).providers;
    } catch {
      providers = [];
    }
  }
  $effect(() => {
    void load();
  });

  function choose(option: LinkOption) {
    active = option;
    tokenId = undefined;
    manualId = '';
    result = {};
  }

  async function search(by: 'id' | 'ip') {
    if (!active) return;
    searching = by;
    result = {};
    try {
      result = await post<SearchResult>(`/server-page/${encodeURIComponent(uuid)}/provider-links/${active.provider}/search`, { by, token_id: tokenId ? Number(tokenId) : null, id: manualId });
    } catch (e) {
      failure(e);
    } finally {
      searching = '';
    }
  }

  async function linkResource() {
    if (!active || !result.matched) return;
    try {
      const r = await post<{ message: string }>(`/server-page/${encodeURIComponent(uuid)}/provider-links/${active.provider}`, { token_id: Number(tokenId), id: result.matched.id });
      notify.ok(r.message);
      active = null;
      open = false;
      await load();
      onlinked();
    } catch (e) {
      failure(e);
    }
  }

  // `ucfirst()`
  const ucfirst = (s: string) => s.charAt(0).toUpperCase() + s.slice(1);
</script>

<svelte:window onclick={(e) => open && menu && !menu.contains(e.target as Node) && (open = false)} />

{#if providers.length}
  <div class="relative" bind:this={menu}>
    <button type="button" class="button" onclick={() => (open = !open)}>
      <Reicon name="plus" class="size-3.5" />
      Link provider
    </button>
    {#if open}
      <div class="absolute top-9 right-0 z-50 w-56 rounded-lg border border-neutral-200 bg-white p-1 shadow-dropdown dark:border-white/[0.1] dark:bg-panel">
        {#each providers as option (option.provider)}
          <button
            type="button"
            onclick={() => choose(option)}
            class="flex h-8 w-full items-center gap-2 rounded-md px-2 text-left text-[12px] text-neutral-600 transition-colors hover:bg-neutral-100 hover:text-black dark:text-fg-dim dark:hover:bg-white/[0.06] dark:hover:text-fg"
          >
            <Reicon name="plus" class="size-3.5" />
            {option.label}
          </button>
        {/each}
      </div>
    {/if}
  </div>
{/if}

{#if active}
  {@const manual = MANUAL[active.provider]}
  <Modal title="Link to {active.label}" onclose={() => (active = null)} focusFirstField={false}>
    <div class="application-settings-form flex flex-col gap-4">
      <p class="text-[12px] leading-5 text-neutral-500 dark:text-fg-dim">
        Link this server to its {active.label} resource for provider status and power controls.
      </p>

      <Listbox id="provider-link-token" label="{active.label} token" placeholder="Select a token" options={active.tokens} bind:value={tokenId} />

      <div class="grid items-end gap-2 sm:grid-cols-[minmax(0,1fr)_auto]">
        <FormInput id="provider-link-manual" label={manual.label} placeholder={manual.placeholder} bind:value={manualId} />
        <button type="button" class="button" disabled={searching === 'id'} onclick={() => search('id')}>
          {searching === 'id' ? 'Searching…' : 'Search ID'}
        </button>
      </div>

      <div class="flex items-center gap-3 text-[10px] uppercase tracking-wide text-neutral-400 dark:text-fg-faint">
        <span class="h-px flex-1 bg-neutral-200 dark:bg-white/[0.08]"></span>
        or
        <span class="h-px flex-1 bg-neutral-200 dark:bg-white/[0.08]"></span>
      </div>

      <button type="button" class="button justify-center" disabled={searching === 'ip'} onclick={() => search('ip')}>
        {searching === 'ip' ? 'Searching…' : 'Search by server IP'}
      </button>

      {#if result.error}
        <!-- Upstream passes type="error", which callout.blade.php does not have: it renders as a warning. -->
        <Callout type="warning" title="Provider search failed">{result.error}</Callout>
      {:else if result.no_match}
        <Callout type="warning" title="No matching resource">Try another token, confirm the resource ID, or verify the server IP.</Callout>
      {:else if result.matched}
        <div class="rounded-lg border border-success/25 bg-success/5 p-3 dark:border-success/20 dark:bg-success/5">
          <div class="flex items-start justify-between gap-3">
            <div class="min-w-0">
              <p class="truncate text-[13px] font-semibold text-black dark:text-fg">{result.matched.name ?? 'Matched resource'}</p>
              <p class="mt-1 text-[11px] text-neutral-500 dark:text-fg-dim">
                {result.matched.id ?? 'Unknown ID'}
                {#if result.matched.status}
                  <span class="px-1 text-neutral-300 dark:text-white/15">·</span>
                  {ucfirst(result.matched.status)}
                {/if}
              </p>
            </div>
            <StatusBadge label="Match found" type="success" />
          </div>
          <button type="button" class="button mt-3 button-highlighted" onclick={linkResource}>Link resource</button>
        </div>
      {/if}
    </div>
  </Modal>
{/if}
