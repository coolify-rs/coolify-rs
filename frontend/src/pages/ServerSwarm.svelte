<script lang="ts">
  // Port of upstream livewire/server/swarm.blade.php.
  import Callout from '../components/Callout.svelte';
  import Listbox from '../components/Listbox.svelte';
  import ServerFrame from '../components/ServerFrame.svelte';
  import SettingsSection from '../components/SettingsSection.svelte';
  import StatusBadge from '../components/StatusBadge.svelte';
  import { get, post, failure } from '../lib/serverApi';
  import { session } from '../lib/session.svelte';
  import { notify } from '../lib/toast.svelte';

  let { uuid }: { uuid: string } = $props();
  type Data = { is_swarm_manager: boolean; is_swarm_worker: boolean; can_use_swarm: boolean };
  let data = $state<Data | null>(null);
  const url = $derived(`/server-page/${encodeURIComponent(uuid)}/swarm`);

  async function load() {
    data = await get<Data>(url);
  }
  async function save() {
    if (!data) return;
    try {
      await post(url, { is_swarm_manager: data.is_swarm_manager, is_swarm_worker: data.is_swarm_worker });
      notify.success('Swarm settings updated.');
    } catch (e) {
      failure(e);
      await load();
    }
  }
</script>

<ServerFrame {uuid} active="swarm" title="Swarm" onloaded={load}>
  {#snippet children()}
    {#if data}
      <div class="application-settings-form w-full">
        <SettingsSection id="server-swarm-section" title="Docker Swarm" description="Legacy Docker Swarm role configuration for this server.">
          {#snippet actions()}
            <StatusBadge label="Deprecated" type="warning" />
          {/snippet}
          <Callout type="warning" title="Docker Swarm support is deprecated">
            Docker Swarm is deprecated and will be removed in Coolify v5. Coolify v5 will be replacing Swarm with native Docker Compose replicas and our own scaling solution. Existing Swarm deployments will continue to work on v4 as-is. We do not recommend setting up new Swarm deployments for the time being.
            <a class="font-medium underline" href="https://coolify.io/docs/knowledge-base/docker/swarm" target="_blank">Read the migration guidance.</a>
          </Callout>
          {#if !data.can_use_swarm}
            <Callout type="info" title="Unavailable for new teams" class="mt-4">Docker Swarm cannot be enabled because this team has no existing Swarm resources.</Callout>
          {/if}
          <div class="mt-4 grid gap-4 lg:grid-cols-2">
            <Listbox
              id="isSwarmManager"
              label="Manager role"
              helper="Managers control scheduling and cluster state."
              bind:value={data.is_swarm_manager}
              onchange={save}
              disabled={!data.can_use_swarm || data.is_swarm_worker || !session.isAdmin}
              options={[
                { value: false, label: 'Not a Swarm manager' },
                { value: true, label: 'Swarm manager' },
              ]}
            />
            <Listbox
              id="isSwarmWorker"
              label="Worker role"
              helper="Workers run tasks assigned by a Swarm manager."
              bind:value={data.is_swarm_worker}
              onchange={save}
              disabled={!data.can_use_swarm || data.is_swarm_manager || !session.isAdmin}
              options={[
                { value: false, label: 'Not a Swarm worker' },
                { value: true, label: 'Swarm worker' },
              ]}
            />
          </div>
        </SettingsSection>
      </div>
    {/if}
  {/snippet}
</ServerFrame>
