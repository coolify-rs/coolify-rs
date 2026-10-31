<script lang="ts">
  // Port of upstream livewire/project/application/swarm.blade.php (+ Application\Swarm).
  import Callout from '../Callout.svelte';
  import FormInput from '../FormInput.svelte';
  import FormTextarea from '../FormTextarea.svelte';
  import Listbox from '../Listbox.svelte';
  import SettingsSection from '../SettingsSection.svelte';
  import UnsavedBar from '../UnsavedBar.svelte';
  import { ApiError, request } from '../../lib/api';
  import { resPath } from '../../lib/sharedApi';
  import { notify } from '../../lib/toast.svelte';

  let { uuid, onchange }: { uuid: string; onchange?: () => void } = $props();

  const placeholder = `placement:
    constraints:
        - 'node.role == worker'`;

  let canUpdate = $state(false);
  let loaded = $state(false);
  let replicas = $state('');
  let constraints = $state('');
  let onlyWorkers = $state(false);
  let saved = $state('');
  let saving = $state(false);
  let error = $state<ApiError | null>(null);

  const path = $derived(`${resPath('application', uuid)}/swarm`);
  const key = () => JSON.stringify([replicas, constraints]);
  const dirty = $derived(JSON.stringify([replicas, constraints]) !== saved);

  async function load() {
    const d = await request<{ can_update: boolean; swarm_replicas: number; swarm_placement_constraints: string | null; is_swarm_only_worker_nodes: boolean }>('GET', path);
    canUpdate = d.can_update;
    replicas = String(d.swarm_replicas ?? '');
    constraints = d.swarm_placement_constraints ?? '';
    onlyWorkers = d.is_swarm_only_worker_nodes;
    saved = key();
    loaded = true;
  }

  $effect(() => {
    load();
  });

  async function save() {
    saving = true;
    error = null;
    try {
      const r = await request<{ message: string }>('POST', path, { swarmReplicas: replicas, swarmPlacementConstraints: constraints, isSwarmOnlyWorkerNodes: onlyWorkers });
      notify.success(r.message);
      saved = key();
      onchange?.();
    } catch (err) {
      const f = err instanceof ApiError ? err : new ApiError(0, String(err));
      if (Object.keys(f.errors).length) error = f;
      else notify.failure(f.message);
    } finally {
      saving = false;
    }
  }
</script>

{#if loaded}
  <form
    class="application-settings-form flex flex-col gap-6"
    onsubmit={(e) => {
      e.preventDefault();
      save();
    }}
  >
    <UnsavedBar {dirty} {saving} onsave={save} onreset={load} />
    <SettingsSection title="Swarm configuration" description="Legacy placement and replica settings for Docker Swarm deployments.">
      {#snippet actions()}
        <span class="inline-flex items-center"><span class="px-2 py-0.5 text-xs font-medium leading-normal rounded-full bg-warning/15 text-warning border border-warning/30">Deprecated</span></span>
      {/snippet}
      <Callout type="warning" title="Deprecated">
        Docker Swarm is deprecated and will be removed in Coolify v5. Coolify v5 will be replacing Swarm with native Docker Compose replicas and our own scaling solution. Existing Swarm deployments will continue to work on v4 as-is. We do not recommend setting up new Swarm deployments for the time being.
      </Callout>

      <div class="mt-4 grid gap-4 lg:grid-cols-2">
        <FormInput id="swarmReplicas" label="Replicas" required disabled={!canUpdate} bind:value={replicas} error={error?.errors.swarmReplicas?.[0]} />
        <Listbox
          id="isSwarmOnlyWorkerNodes"
          label="Node placement"
          disabled={!canUpdate}
          options={[
            { value: true, label: 'Worker nodes only' },
            { value: false, label: 'Manager and worker nodes' },
          ]}
          bind:value={onlyWorkers}
          onchange={save}
        />
        <div class="lg:col-span-2">
          <FormTextarea id="swarmPlacementConstraints" rows={8} label="Custom placement constraints" {placeholder} disabled={!canUpdate} bind:value={constraints} />
        </div>
      </div>
    </SettingsSection>
  </form>
{/if}
