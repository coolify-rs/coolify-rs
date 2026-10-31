<script lang="ts">
  // Port of upstream livewire/destination/new/docker.blade.php.
  import FormInput from './FormInput.svelte';
  import Listbox from './Listbox.svelte';
  import { ApiError } from '../lib/api';
  import { post, failure, kebab, type DestinationListData } from '../lib/serverApi';
  import { router } from '../lib/router.svelte';

  let {
    servers,
    network: initialNetwork,
    serverId: initialServer,
    oncreated,
  }: {
    servers: DestinationListData['servers'];
    network: string;
    serverId?: string;
    oncreated?: (uuid: string) => void;
  } = $props();

  let network = $state(initialNetwork);
  let serverId = $state(initialServer ?? String(servers[0]?.id ?? ''));
  /** `generateName`: runs on mount and when the server changes. */
  const generated = (id: string) => kebab(`${servers.find((s) => String(s.id) === id)?.name ?? ''}-${network}`);
  let name = $state(generated(initialServer ?? String(servers[0]?.id ?? '')));
  let error = $state<ApiError | null>(null);
  let saving = $state(false);

  async function submit(e: Event) {
    e.preventDefault();
    saving = true;
    error = null;
    try {
      const r = await post<{ uuid: string }>('/destination-list', { name, network, serverId });
      if (oncreated) oncreated(r.uuid);
      else router.go(`/destination/${r.uuid}`);
    } catch (err) {
      error = failure(err);
    } finally {
      saving = false;
    }
  }
</script>

<div class="w-full">
  <form class="flex w-full flex-col gap-4" onsubmit={submit}>
    <div class="grid gap-4 sm:grid-cols-2">
      <FormInput id="name" label="Name" required bind:value={name} error={error?.field('name')} />
      <FormInput id="network" label="Network" required bind:value={network} error={error?.field('network')} />
    </div>
    <Listbox
      id="serverId"
      label="Server"
      required
      bind:value={serverId}
      onchange={(v) => (name = generated(v))}
      options={servers.map((s) => ({ value: String(s.id), label: s.name }))}
    />
    <div class="flex justify-end border-t border-neutral-200 pt-4 dark:border-white/[0.07]">
      <button type="submit" class="button button-highlighted" disabled={saving}>Create destination</button>
    </div>
  </form>
</div>
