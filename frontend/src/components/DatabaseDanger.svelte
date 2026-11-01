<script lang="ts">
  // Port of upstream livewire/project/shared/danger.blade.php (+ Shared\Danger)
  // for a standalone database, an application or a service.
  import Callout from './Callout.svelte';
  import DangerZone from './DangerZone.svelte';
  import ConfirmModal from './ConfirmModal.svelte';
  import SettingsSection from './SettingsSection.svelte';
  import { dbUi, appUi, ApiError, request } from '../lib/api';
  import { router } from '../lib/router.svelte';
  import { notify } from '../lib/toast.svelte';

  let {
    record,
    canDelete,
    back,
    kind = 'database',
    flushable = false,
  }: {
    record: { uuid: string; name: string | null; server_status?: unknown };
    canDelete: boolean;
    back: string;
    kind?: 'database' | 'application' | 'service';
    flushable?: boolean;
  } = $props();

  let deleting = $state(false);
  let flushing = $state(false);
  const resourceName = $derived(record.name ?? (kind === 'application' ? 'Application' : kind === 'service' ? 'Service' : 'Database'));
  // `$resourceLabel` of the view.
  const resourceLabel = $derived(kind === 'application' ? 'application' : kind === 'service' ? 'service' : 'resource');
  const functional = $derived(record.server_status !== false);

  // Livewire `Shared\Danger`: every option starts selected. Connected networks only
  // matter for applications, so the database delete ignores that option, as upstream does.
  const checkboxes = [
    { id: 'delete_volumes', label: 'Permanently delete all volumes associated with this resource.', checked: true },
    { id: 'delete_connected_networks', label: 'Permanently delete all non-predefined networks associated with this resource.', checked: true },
    { id: 'delete_configurations', label: 'Permanently delete all configuration files from the server.', checked: true },
    { id: 'docker_cleanup', label: 'Run Docker Cleanup (remove unused images and builder cache).', checked: true },
  ];

  // `Shared\Danger::flush($password)`.
  async function flush(password: string): Promise<string | void> {
    try {
      const r = await request<{ error?: string }>('POST', `/ui/databases/${encodeURIComponent(record.uuid)}/flush`, { password });
      if (r.error) notify.error(r.error);
      else notify.ok('Database flushed.');
    } catch (e) {
      if (e instanceof ApiError && e.message === 'The provided password is incorrect.') return e.message;
      throw e;
    }
  }

  async function remove(password: string, selected: string[]): Promise<string | void> {
    try {
      const body = {
        password,
        delete_volumes: selected.includes('delete_volumes'),
        delete_configurations: selected.includes('delete_configurations'),
        docker_cleanup: selected.includes('docker_cleanup'),
      };
      if (kind === 'application') await appUi.remove(record.uuid, { ...body, delete_connected_networks: selected.includes('delete_connected_networks') });
      else if (kind === 'service')
        await request('POST', `/ui/resources/service/${encodeURIComponent(record.uuid)}/delete`, { ...body, delete_connected_networks: selected.includes('delete_connected_networks') });
      else await dbUi.remove(record.uuid, body);
      router.go(back);
    } catch (e) {
      if (e instanceof ApiError && e.message === 'The provided password is incorrect.') return e.message;
      throw e;
    }
  }
</script>

<div>
  {#if !functional}
    <Callout type="warning" title="Server is not reachable" class="mb-4">
      Coolify cannot stop or remove Docker resources on this server. Deleting removes this {resourceLabel} from Coolify only. Its containers, volumes, networks, and
      configuration files remain on the server and can start again when the server is back.
    </Callout>
  {/if}
  <SettingsSection id="danger-zone-section" title="Danger zone" description="Destructive resource actions cannot be undone.">
    {#if flushable}
      <DangerZone title="Flush all data" class="mb-4">
        <p>
          Permanently remove every key from
          <strong class="font-semibold text-black dark:text-fg">{resourceName}</strong>.
          The database keeps running.
        </p>
        {#snippet action()}
          {#if canDelete}
            <div class="relative h-auto max-w-full inline-flex w-auto">
              <button class="button button-error" type="button" onclick={() => (flushing = true)}>Flush all data</button>
            </div>
          {:else}
            <button class="button button-error" type="button" disabled title="You do not have permission to flush this database.">Flush all data</button>
          {/if}
        {/snippet}
      </DangerZone>
    {/if}
    <DangerZone title="Delete {resourceLabel}">
      <p>
        Permanently delete
        <strong class="font-semibold text-black dark:text-fg">{resourceName}</strong>, stop its containers, and remove the selected Docker resources and
        configuration.
      </p>
      <ul class="space-y-1 text-xs">
        <li>• Active deployments will be stopped.</li>
        <li>• Selected volumes and stored data may be permanently removed.</li>
        <li>• This {resourceLabel} cannot be restored from Coolify after deletion.</li>
      </ul>
      {#snippet action()}
        {#if canDelete}
          <div class="relative h-auto max-w-full inline-flex w-auto">
            <button class="button button-error" type="button" onclick={() => (deleting = true)}>Delete {resourceLabel}</button>
          </div>
        {:else}
          <button class="button button-error" type="button" disabled title="You do not have permission to delete this resource.">Delete {resourceLabel}</button>
        {/if}
      {/snippet}
    </DangerZone>

    {#if !canDelete}
      <div class="mt-4">
        <Callout type="danger" title="Insufficient permissions">Contact a team administrator if this resource must be deleted.</Callout>
      </div>
    {/if}
  </SettingsSection>
</div>

{#if flushing}
  <ConfirmModal
    title="Flush all data?"
    actions={['Permanently remove every key from every logical database.']}
    confirmWithText
    confirmWithPassword
    confirmationText={resourceName}
    confirmationLabel="Enter the database name to confirm flushing all data"
    shortConfirmationLabel="Database name"
    onconfirm={flush}
    onclose={() => (flushing = false)}
  />
{/if}

{#if deleting}
  <ConfirmModal
    title="Delete {resourceLabel}?"
    actions={['Permanently delete this resource and its selected Docker resources.']}
    {checkboxes}
    confirmWithText
    confirmWithPassword
    confirmationText={resourceName}
    confirmationLabel="Enter the resource name to confirm permanent deletion"
    shortConfirmationLabel="Resource name"
    onconfirm={remove}
    onclose={() => (deleting = false)}
  />
{/if}
