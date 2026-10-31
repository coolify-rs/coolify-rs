<script lang="ts">
  // Port of upstream livewire/server/destinations.blade.php.
  import Reicon from '../components/Reicon.svelte';
  import Callout from '../components/Callout.svelte';
  import Empty from '../components/Empty.svelte';
  import Modal from '../components/Modal.svelte';
  import NewDestination from '../components/NewDestination.svelte';
  import ServerFrame from '../components/ServerFrame.svelte';
  import SettingsSection from '../components/SettingsSection.svelte';
  import type { ServerFacts } from '../lib/api';
  import { get, post, failure } from '../lib/serverApi';
  import { link, router } from '../lib/router.svelte';
  import { session } from '../lib/session.svelte';
  import { notify } from '../lib/toast.svelte';

  let { uuid }: { uuid: string } = $props();
  type Data = {
    destinations: { uuid: string; network: string; is_swarm: boolean }[];
    network: string;
    server: { id: number; uuid: string; name: string };
  };
  let data = $state<Data | null>(null);
  let networks = $state<string[]>([]);
  let adding = $state(false);
  const base = $derived(`/server-page/${encodeURIComponent(uuid)}/destinations`);
  const canEdit = $derived(session.isAdmin);

  async function load(_?: ServerFacts) {
    data = await get<Data>(base);
  }
  async function scan() {
    try {
      const r = await post<{ message: string; networks: string[] }>(`${base}/scan`);
      networks = r.networks;
      notify.success(r.message);
    } catch (e) {
      failure(e);
    }
  }
  async function add(name: string) {
    try {
      await post(`${base}/add`, { name });
      networks = networks.filter((n) => n !== name);
      await load();
    } catch (e) {
      failure(e);
    }
  }
</script>

<ServerFrame {uuid} active="destinations" title="Destinations" onloaded={load}>
  {#snippet children(facts)}
    <div class="application-settings-form flex w-full flex-col gap-6">
      {#if facts.is_swarm}
        <Callout type="warning" title="Docker Swarm support is deprecated">
          Docker Swarm is deprecated and will be removed in Coolify v5. Coolify v5 will be replacing Swarm with native Docker Compose replicas and our own scaling solution. Existing Swarm deployments will continue to work on v4 as-is. We do not recommend setting up new Swarm deployments for the time being.
        </Callout>
      {/if}
      {#if facts.functional}
        <SettingsSection id="server-destinations-section" title="Destinations" description="Docker networks used to isolate and connect resources on this server." flush>
          {#snippet actions()}
            <div class="flex items-center gap-2">
              <button type="button" class="button" disabled={!canEdit} onclick={scan}>
                <Reicon name="refresh" class="size-3.5" />
                Scan networks
              </button>
              {#if canEdit}
                <button type="button" class="button" onclick={() => (adding = true)}>+ Add</button>
              {/if}
            </div>
          {/snippet}
          {#if data}
            {#each data.destinations as d (d.uuid)}
              <a
                href="/destination/{d.uuid}"
                use:link
                class="flex items-center gap-4 border-b border-neutral-200 px-4 py-3 transition-colors last:border-b-0 hover:bg-neutral-50 dark:border-white/[0.08] dark:hover:bg-white/[0.03]"
              >
                <div class="flex size-9 shrink-0 items-center justify-center rounded-lg bg-neutral-100 text-neutral-500 dark:bg-white/[0.06] dark:text-fg-dim">
                  <Reicon name="destinations" class="size-4" />
                </div>
                <div class="min-w-0 flex-1">
                  <p class="truncate text-sm font-medium text-neutral-950 dark:text-fg">{d.network}</p>
                  <p class="mt-0.5 text-xs text-neutral-500 dark:text-fg-dim">{d.is_swarm ? 'Docker Swarm' : 'Standalone Docker'}</p>
                </div>
              </a>
            {:else}
              <Empty size="sm" title="No destinations" description="Add a destination or scan the server for existing Docker networks." iconName="destinations" />
            {/each}
          {/if}
        </SettingsSection>
        {#if networks.length > 0}
          <SettingsSection id="server-found-networks-section" title="Discovered networks" description="Networks found on the server that are not registered as Coolify destinations." flush>
            {#each networks as name (name)}
              <div class="flex items-center justify-between gap-4 border-b border-neutral-200 px-4 py-3 last:border-b-0 dark:border-white/[0.08]">
                <div>
                  <p class="text-sm font-medium text-neutral-950 dark:text-fg">{name}</p>
                  <p class="mt-0.5 text-xs text-neutral-500 dark:text-fg-dim">Docker network</p>
                </div>
                <button type="button" class="button" disabled={!canEdit} onclick={() => add(name)}>Add destination</button>
              </div>
            {/each}
          </SettingsSection>
        {/if}
      {:else}
        <SettingsSection title="Destinations" description="Docker networks used to isolate and connect resources on this server.">
          <Empty size="sm" title="Server validation required" description="Validate this server before managing its destinations." iconName="destinations" />
        </SettingsSection>
      {/if}
    </div>
  {/snippet}
</ServerFrame>

{#if adding && data}
  <Modal title="New Destination" onclose={() => (adding = false)}>
    <NewDestination
      servers={[{ id: data.server.id, uuid: data.server.uuid, name: data.server.name }]}
      network={data.network}
      serverId={String(data.server.id)}
      oncreated={(u) => router.go(`/destination/${u}`)}
    />
  </Modal>
{/if}
