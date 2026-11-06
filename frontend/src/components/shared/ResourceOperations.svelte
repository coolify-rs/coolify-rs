<script lang="ts">
  // Port of upstream livewire/project/shared/resource-operations.blade.php
  // (+ Project\Shared\ResourceOperations). The "Migrate to another server" section
  // only renders in a Coolify development instance (`isDev()`).
  import Callout from '../Callout.svelte';
  import Checkbox from '../Checkbox.svelte';
  import ConfirmModal from '../ConfirmModal.svelte';
  import Empty from '../Empty.svelte';
  import Listbox from '../Listbox.svelte';
  import SettingsSection from '../SettingsSection.svelte';
  import { ApiError, request } from '../../lib/api';
  import { router } from '../../lib/router.svelte';
  import { resPath, type ResourceKind } from '../../lib/sharedApi';
  import { notify } from '../../lib/toast.svelte';

  let { kind, uuid }: { kind: ResourceKind; uuid: string } = $props();

  interface Destination {
    id: number;
    uuid: string;
    name: string;
  }
  interface Server {
    id: number;
    name: string;
    ip: string;
    is_functional: boolean;
    destinations: Destination[];
  }
  interface Project {
    id: number;
    name: string;
    environments: { id: number; name: string; project_id: number }[];
  }
  interface Data {
    can_update: boolean;
    current_project_id: number;
    current_environment_id: number;
    current_server_id: number;
    current_destination_uuid: string;
    servers: Server[];
    build_servers: { id: number; name: string }[];
    projects: Project[];
  }

  let data = $state<Data | null>(null);
  let cloneServer = $state('');
  let cloneDestination = $state('');
  let cloneProject = $state('');
  let cloneEnvironment = $state('');
  let cloneVolumeData = $state(false);
  let confirmClone = $state(false);
  let moveProject = $state('');
  let moveEnvironment = $state('');

  const base = $derived(`${resPath(kind, uuid)}/operations`);

  async function load() {
    const d = await request<Data>('GET', base);
    data = d;
    cloneServer = String(d.current_server_id);
    cloneDestination = d.current_destination_uuid;
    cloneProject = String(d.current_project_id);
    cloneEnvironment = String(d.current_environment_id);
    moveProject = '';
    moveEnvironment = '';
  }

  $effect(() => {
    load();
  });

  const availableDestinations = $derived(data?.servers.find((s) => String(s.id) === cloneServer)?.destinations ?? []);
  const availableCloneEnvironments = $derived(data?.projects.find((p) => String(p.id) === cloneProject)?.environments ?? []);
  const availableEnvironments = $derived.by(() => {
    const project = data?.projects.find((p) => String(p.id) === moveProject);
    if (!project || !data) return [];
    return project.environments.filter((e) => (project.id === data!.current_project_id ? e.id !== data!.current_environment_id : true));
  });

  const cloneServerOptions = $derived([
    ...(data?.servers ?? []).map((s) => ({
      value: String(s.id),
      label: `${s.name} (${s.ip})${s.id === data?.current_server_id ? ' (current)' : ''}`,
    })),
    ...(data?.build_servers ?? []).map((s) => ({ value: `build-${s.id}`, label: `${s.name} (build server)`, disabled: true })),
  ]);
  const cloneDestinationOptions = $derived(
    availableDestinations.map((d) => ({ value: d.uuid, label: d.name + (d.uuid === data?.current_destination_uuid ? ' (current)' : '') })),
  );
  const projectOptions = $derived(
    (data?.projects ?? []).map((p) => ({ value: String(p.id), label: p.name + (p.id === data?.current_project_id ? ' (current)' : '') })),
  );
  const cloneEnvironmentOptions = $derived(
    availableCloneEnvironments.map((e) => ({ value: String(e.id), label: e.name + (e.id === data?.current_environment_id ? ' (current)' : '') })),
  );
  const moveEnvironmentOptions = $derived(availableEnvironments.map((e) => ({ value: String(e.id), label: e.name })));

  function failure(err: unknown) {
    notify.failure(err instanceof ApiError ? err.message : String(err));
  }

  // `redirect()->to($route.'#resource-operations')`.
  function redirect(to: string) {
    router.go(to);
    history.replaceState(history.state, '', `${to}#resource-operations`);
  }

  async function cloneTo() {
    try {
      const r = await request<{ redirect: string }>('POST', `${base}/clone`, {
        destination_uuid: cloneDestination,
        environment_id: Number(cloneEnvironment),
        clone_volume_data: cloneVolumeData,
      });
      redirect(r.redirect);
    } catch (err) {
      failure(err);
    }
  }

  async function moveTo() {
    try {
      const r = await request<{ redirect: string }>('POST', `${base}/move`, { environment_id: Number(moveEnvironment) });
      redirect(r.redirect);
    } catch (err) {
      failure(err);
    }
  }
</script>

{#if data}
  <div class="flex flex-col gap-6">
    {#if data.can_update}
      <SettingsSection
        id="clone-resource-section"
        title="Clone resource"
        description="Create a copy of this resource. Choose where it runs and where it belongs. The current values are selected by default."
      >
        <Callout type="info" title="What is copied">
          Cloning copies settings, environment variables, and resource configuration. Stored files,
          database records, and other persistent data are copied only when you select Clone volume data.
        </Callout>

        <div class="mt-4 grid gap-4 md:grid-cols-2">
          <Listbox
            id="clone-resource-server"
            label="Server"
            options={cloneServerOptions}
            bind:value={cloneServer}
            placeholder="Choose a server…"
            onchange={() => (cloneDestination = '')}
          />
          <Listbox
            id="clone-resource-destination"
            label="Network destination"
            options={cloneDestinationOptions}
            bind:value={cloneDestination}
            disabled={cloneServer === ''}
            placeholder="Choose a destination…"
            emptyText="No network destinations are available on this server."
          />
          <Listbox
            id="clone-resource-project"
            label="Project"
            options={projectOptions}
            bind:value={cloneProject}
            placeholder="Choose a project…"
            onchange={() => (cloneEnvironment = '')}
          />
          <Listbox
            id="clone-resource-environment"
            label="Environment"
            options={cloneEnvironmentOptions}
            bind:value={cloneEnvironment}
            disabled={!cloneProject || availableCloneEnvironments.length === 0}
            placeholder="Choose an environment…"
          />
        </div>

        <div class="mt-4">
          <Checkbox
            id="cloneVolumeData"
            label="Clone volume data"
            helper="Copies the data of the persistent volumes to the clone. A running source resource is stopped while the data is copied and started again afterwards."
            bind:checked={cloneVolumeData}
          />
        </div>

        <div class="mt-4 flex flex-col gap-3 border-t border-neutral-200 pt-4 sm:flex-row sm:items-center sm:justify-between dark:border-white/[0.07]">
          {#if cloneVolumeData}
            <p class="text-[13px] text-neutral-500 dark:text-fg-dim">A running resource is stopped while its volume data is copied.</p>
            <button type="button" class="button" disabled={!cloneDestination || !cloneEnvironment} onclick={() => (confirmClone = true)}>Clone resource</button>
          {:else}
            <p class="text-[13px] text-neutral-500 dark:text-fg-dim">The running resource will not be changed.</p>
            <button type="button" class="button" disabled={!cloneDestination || !cloneEnvironment} onclick={cloneTo}>Clone resource</button>
          {/if}
        </div>
      </SettingsSection>

      <SettingsSection
        id="move-resource-section"
        title="Move resource"
        description="Transfer this resource to another project environment without changing the running deployment."
      >
        {#if data.projects.length > 0}
          <div class="grid gap-4 md:grid-cols-2">
            <Listbox
              id="move-resource-project"
              label="Project"
              options={projectOptions}
              bind:value={moveProject}
              placeholder="Choose a project…"
              onchange={() => (moveEnvironment = '')}
            />
            <Listbox
              id="move-resource-environment"
              label="Environment"
              helper="The current environment is excluded."
              options={moveEnvironmentOptions}
              bind:value={moveEnvironment}
              disabled={!moveProject || availableEnvironments.length === 0}
              placeholder="Choose an environment…"
            />
          </div>

          {#if moveEnvironment}
            <div class="mt-4 flex flex-col gap-3 border-t border-neutral-200 pt-4 sm:flex-row sm:items-center sm:justify-between dark:border-white/[0.07]">
              <p class="text-[13px] text-neutral-500 dark:text-fg-dim">All configuration will move with the resource.</p>
              <button type="button" class="button" onclick={moveTo}>Move resource</button>
            </div>
          {/if}
        {:else}
          <Empty size="sm" title="No destination environments" description="Create another project or environment before moving this resource." iconName="projects" />
        {/if}
      </SettingsSection>
    {:else}
      <Callout type="danger" title="Insufficient permissions">
        You do not have permission to clone or move this resource. Contact a team administrator for access.
      </Callout>
    {/if}
  </div>
{/if}

{#if confirmClone}
  <ConfirmModal
    title="Clone this resource with its volume data?"
    actions={['If the resource is running, it is stopped while its volume data is copied.', 'The resource is started again after the copy, also when the copy fails.']}
    warningMessage="A running resource is unavailable while its volume data is copied."
    confirmWithText={false}
    step2ButtonText="Clone resource"
    onconfirm={async () => {
      confirmClone = false;
      await cloneTo();
    }}
    onclose={() => (confirmClone = false)}
  />
{/if}
