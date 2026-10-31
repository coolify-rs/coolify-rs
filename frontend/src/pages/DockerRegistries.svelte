<script lang="ts">
  // Port of upstream livewire/server/docker-registries.blade.php.
  import Empty from '../components/Empty.svelte';
  import Modal from '../components/Modal.svelte';
  import RegistryCard from '../components/RegistryCard.svelte';
  import RegistryLogin from '../components/RegistryLogin.svelte';
  import { serversUi, type ServersPageData } from '../lib/api';

  let data = $state<ServersPageData | null>(null);
  let logging = $state(false);
  let generation = $state(0);
  serversUi.list().then((d) => (data = d));
</script>

<div class="w-full">
  <div class="mb-5 flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
    <div class="flex min-w-0 flex-col gap-1">
      <h1 class="min-w-0 text-[24px]! leading-7! font-semibold! tracking-tight!">Docker Registries</h1>
      <p class="text-[12px] text-neutral-500 dark:text-fg-dim">
        Registry logins on each server, read from the Docker config that deployments use. Use <b>Multi-server login</b>, or run <code>docker login &lt;registry&gt;</code> on a server.
      </p>
    </div>
    {#if data && data.servers.length > 0}
      <button type="button" class="button button-highlighted" onclick={() => (logging = true)}>Multi-server login</button>
    {/if}
  </div>
  {#if data && data.servers.length === 0}
    <Empty title="No servers yet" description="Add a server to manage its Docker registry logins." iconName="servers" />
  {:else if data}
    <div class="flex flex-col gap-4">
      {#key generation}
        {#each data.servers as s (s.uuid)}
          <RegistryCard server={s} />
        {/each}
      {/key}
    </div>
  {/if}
</div>

{#if logging && data}
  <Modal title="Log in on multiple servers" subtitle="Runs docker login on each server that you choose." onclose={() => (logging = false)}>
    <RegistryLogin picker servers={data.servers.filter((s) => s.functional)} ondone={() => ((logging = false), generation++)} />
  </Modal>
{/if}
