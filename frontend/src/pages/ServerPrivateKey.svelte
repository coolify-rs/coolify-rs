<script lang="ts">
  // Port of upstream livewire/server/private-key/show.blade.php.
  import Reicon from '../components/Reicon.svelte';
  import AddPrivateKeyModal from '../components/AddPrivateKeyModal.svelte';
  import Empty from '../components/Empty.svelte';
  import ServerFrame from '../components/ServerFrame.svelte';
  import SettingsSection from '../components/SettingsSection.svelte';
  import StatusBadge from '../components/StatusBadge.svelte';
  import { get, post, failure } from '../lib/serverApi';
  import { session } from '../lib/session.svelte';
  import { router } from '../lib/router.svelte';
  import { notify } from '../lib/toast.svelte';
  import { sanitizeHtml } from '../lib/sanitize';
  import { ApiError } from '../lib/api';

  let { uuid }: { uuid: string } = $props();
  type Key = { id: number; uuid: string; name: string; description: string | null; public_key: string; active: boolean };
  let keys = $state<Key[] | null>(null);
  let transferred = $state(false);
  let menu = $state(false);
  let adding = $state(false);
  let root: HTMLDivElement | undefined = $state();
  const base = $derived(`/server-page/${encodeURIComponent(uuid)}/private-key`);
  const canEdit = $derived(session.isAdmin);

  async function load() {
    const d = await get<{ keys: Key[]; transferred: boolean }>(base);
    keys = d.keys;
    transferred = d.transferred;
  }

  async function copy(publicKey: string) {
    try {
      await navigator.clipboard.writeText(publicKey);
      notify.success('Public key copied to clipboard.');
    } catch {
      notify.failure('Failed to copy public key to clipboard.');
    }
  }

  async function checkConnection() {
    try {
      const r = await post<{ message: string }>(`${base}/check`);
      notify.success(r.message);
    } catch (e) {
      if (e instanceof ApiError) notify.failure(sanitizeHtml(e.message));
      else failure(e);
    }
  }

  async function generate(type: 'ed25519' | 'rsa') {
    menu = false;
    try {
      const r = await post<{ message: string; public_key: string | null }>(`${base}/generate`, { type });
      await load();
      if (r.public_key) await copy(r.public_key);
      notify.success(r.message);
    } catch (e) {
      failure(e);
    }
  }

  async function useKey(id: number) {
    try {
      const r = await post<{ message: string }>(`${base}/use`, { id });
      await load();
      notify.success(r.message);
    } catch (e) {
      failure(e);
      await load();
    }
  }
</script>

<svelte:window onclick={(e) => menu && root && !root.contains(e.target as Node) && (menu = false)} onkeydown={(e) => e.key === 'Escape' && (menu = false)} />

<ServerFrame {uuid} active="private-key" title="Private Key" onloaded={load}>
  {#snippet children()}
    <div class="application-settings-form flex w-full flex-col gap-6">
      <SettingsSection id="server-private-keys-section" title="Private key" description="Choose the SSH key coolify-rs uses to connect to this server." flush>
        {#snippet actions()}
          <div class="flex items-center gap-2">
            <button type="button" class="button" disabled={!canEdit} onclick={checkConnection}>
              <Reicon name="refresh" class="size-3.5" />
              Check connection
            </button>
            {#if canEdit}
              <div class="relative" bind:this={root}>
                <button type="button" class="button" {...{ ishighlighted: '' }} aria-haspopup="menu" aria-expanded={menu} onclick={() => (menu = !menu)}>
                  <Reicon name="plus" class="size-3.5" />
                  Add
                  <Reicon name="chevron-down" class="size-3 opacity-55" />
                </button>
                {#if menu}
                  <div role="menu" class="listbox-panel left-auto! right-0! z-[90]! w-52! min-w-52!">
                    <button type="button" class="listbox-option justify-start! gap-2.5!" role="menuitem" onclick={() => generate('ed25519')}>
                      <Reicon name="keys" class="size-3.5 shrink-0 opacity-70" />
                      Generate ED25519
                    </button>
                    <button type="button" class="listbox-option justify-start! gap-2.5!" role="menuitem" onclick={() => generate('rsa')}>
                      <Reicon name="keys" class="size-3.5 shrink-0 opacity-70" />
                      Generate RSA
                    </button>
                    <button type="button" class="listbox-option justify-start! gap-2.5! w-full" role="menuitem" onclick={() => ((menu = false), (adding = true))}>
                      <Reicon name="plus" class="size-3.5 shrink-0 opacity-70" />
                      Add manually
                    </button>
                  </div>
                {/if}
              </div>
            {/if}
          </div>
        {/snippet}

        {#if keys}
          {#each keys as key (key.id)}
            <div class="flex flex-col gap-4 border-b border-neutral-200 px-4 py-4 last:border-b-0 sm:flex-row sm:items-center sm:justify-between dark:border-white/[0.08]">
              <div class="flex min-w-0 items-start gap-3">
                <div class="flex size-9 shrink-0 items-center justify-center rounded-lg bg-neutral-100 text-neutral-500 dark:bg-white/[0.06] dark:text-fg-dim">
                  <Reicon name="keys" class="size-4" />
                </div>
                <div class="min-w-0 self-center">
                  <div class="flex flex-wrap items-center gap-2">
                    <p class="truncate text-sm font-medium text-neutral-950 dark:text-fg">{key.name}</p>
                    {#if key.active}
                      <StatusBadge label="Active" type="success" />
                    {/if}
                  </div>
                  {#if key.description}
                    <p class="mt-1 text-xs text-neutral-500 dark:text-fg-dim">{key.description}</p>
                  {/if}
                </div>
              </div>
              <div class="flex shrink-0 items-center gap-2">
                <button type="button" class="button" onclick={() => copy(key.public_key)}>Copy public key</button>
                {#if !key.active && !transferred}
                  <button type="button" class="button" disabled={!canEdit} onclick={() => useKey(key.id)}>Use this key</button>
                {/if}
              </div>
            </div>
          {:else}
            <Empty size="sm" title="No private keys" description="Add or generate a private key to connect to this server." iconName="keys" />
          {/each}
        {/if}
      </SettingsSection>
    </div>
  {/snippet}
</ServerFrame>

{#if adding}
  <AddPrivateKeyModal onclose={() => (adding = false)} oncreated={(key) => router.go(`/security/private-key/${key.uuid}`)} />
{/if}
