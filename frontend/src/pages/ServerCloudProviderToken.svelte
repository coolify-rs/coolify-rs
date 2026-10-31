<script lang="ts">
  // Port of upstream livewire/server/cloud-provider-token/show.blade.php.
  import CloudTokenForm from '../components/CloudTokenForm.svelte';
  import Empty from '../components/Empty.svelte';
  import Reicon from '../components/Reicon.svelte';
  import ServerFrame from '../components/ServerFrame.svelte';
  import SettingsSection from '../components/SettingsSection.svelte';
  import StatusBadge from '../components/StatusBadge.svelte';
  import { type CloudToken } from '../lib/cloudApi';
  import { diffForHumans } from '../lib/format';
  import { action, get, post } from '../lib/serverApi';
  import { session } from '../lib/session.svelte';

  type Page = { managed: boolean; provider: string; provider_name: string; current_token_id: number | null; tokens: CloudToken[] };
  let { uuid }: { uuid: string } = $props();
  let page = $state<Page | null>(null);
  let adding = $state(false);
  const base = $derived(`/server-page/${encodeURIComponent(uuid)}/cloud-provider-token`);
  const canUpdate = $derived(session.isAdmin);

  async function load() {
    page = await get<Page>(base);
  }

  async function validateToken() {
    await action(() => post(`${base}/validate`));
  }

  async function useToken(id: number) {
    const r = await action(() => post(base, { token_id: id }));
    if (r) await load();
  }
</script>

<ServerFrame {uuid} active="cloud-provider-token" title="Cloud Token" onloaded={load}>
  {#snippet children()}
    <div class="application-settings-form w-full">
      {#if page?.managed}
        <SettingsSection id="server-cloud-token-section" title="{page.provider_name} token" description="Choose the cloud credential used to manage this server." flush>
          {#snippet actions()}
            <div class="flex items-center gap-2">
              <button type="button" class="button" disabled={!canUpdate} onclick={validateToken}>
                <Reicon name="refresh" class="size-3.5" />
                Validate token
              </button>
              {#if canUpdate}
                <button type="button" class="button" onclick={() => (adding = true)}>+ Add</button>
              {/if}
            </div>
          {/snippet}

          {#each page.tokens as token (token.id)}
            <div class="flex items-center gap-4 border-b border-neutral-200 px-4 py-3 last:border-b-0 dark:border-white/[0.08]">
              <div class="flex size-9 shrink-0 items-center justify-center rounded-lg bg-neutral-100 text-neutral-500 dark:bg-white/[0.06] dark:text-fg-dim">
                <Reicon name="keys" class="size-4" />
              </div>
              <div class="min-w-0 flex-1">
                <div class="flex flex-wrap items-center gap-2">
                  <p class="truncate text-sm font-medium text-neutral-950 dark:text-fg">{token.name}</p>
                  {#if page.current_token_id === token.id}
                    <StatusBadge label="Active" type="success" />
                  {/if}
                </div>
                <p class="mt-0.5 text-xs text-neutral-500 dark:text-fg-dim">
                  {token.description || `Created ${diffForHumans(token.created_at)}`}
                </p>
              </div>
              {#if page.current_token_id !== token.id}
                <button type="button" class="button" disabled={!canUpdate} onclick={() => useToken(token.id)}>Use this token</button>
              {/if}
            </div>
          {:else}
            <Empty size="sm" title="No {page.provider_name} tokens" description="Add a token to manage this server through {page.provider_name}." iconName="keys" />
          {/each}
        </SettingsSection>
      {:else if page}
        <SettingsSection title="Cloud token" description="Cloud credentials are available for servers created through a supported provider.">
          <Empty
            size="sm"
            title="No cloud provider integration"
            description="This server was not created through a supported cloud provider, so it does not require a managed cloud token."
            iconName="keys"
          />
        </SettingsSection>
      {/if}
    </div>
  {/snippet}
</ServerFrame>

{#if adding && page}
  <CloudTokenForm
    provider={page.provider}
    providerLabel={page.provider_name}
    onclose={() => (adding = false)}
    oncreated={async () => {
      adding = false;
      // `getListeners()` has no `tokenAdded.vultr`: upstream's list stays as it is until the next visit.
      if (page?.provider !== 'vultr') await load();
    }}
  />
{/if}
