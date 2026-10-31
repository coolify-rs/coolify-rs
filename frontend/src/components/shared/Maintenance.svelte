<script lang="ts">
  // Port of upstream livewire/project/shared/maintenance.blade.php (+ Shared\Maintenance).
  import FormTextarea from '../FormTextarea.svelte';
  import Listbox from '../Listbox.svelte';
  import SettingsSection from '../SettingsSection.svelte';
  import UnsavedBar from '../UnsavedBar.svelte';
  import { ApiError, request } from '../../lib/api';
  import { resPath, type ResourceKind } from '../../lib/sharedApi';
  import { notify } from '../../lib/toast.svelte';

  let { kind, uuid }: { kind: ResourceKind; uuid: string } = $props();

  let data = $state<{ can_update: boolean; is_supported: boolean } | null>(null);
  let enabled = $state(false);
  let page = $state('');
  let saved = $state('');
  let saving = $state(false);

  const base = $derived(`${resPath(kind, uuid)}/maintenance`);

  async function load() {
    const d = await request<{ can_update: boolean; is_supported: boolean; is_maintenance_enabled: boolean; maintenance_page: string | null }>('GET', base);
    data = d;
    enabled = d.is_maintenance_enabled;
    page = saved = d.maintenance_page ?? '';
  }

  $effect(() => {
    load();
  });

  const failure = (err: unknown) => notify.failure(err instanceof ApiError ? err.message : String(err));

  // `instantSaveMaintenance`.
  async function toggle() {
    try {
      const r = await request<{ message: string }>('POST', `${base}/toggle`, { enabled });
      notify.success(r.message);
    } catch (err) {
      failure(err);
      await load();
    }
  }

  async function submit() {
    saving = true;
    try {
      const r = await request<{ message: string }>('POST', base, { maintenancePage: page });
      notify.success(r.message);
      saved = page;
    } catch (err) {
      failure(err);
    } finally {
      saving = false;
    }
  }

  async function reset() {
    try {
      const r = await request<{ message: string }>('POST', `${base}/reset`, {});
      notify.success(r.message);
      page = saved = '';
    } catch (err) {
      failure(err);
    }
  }
</script>

{#if data}
  <form
    onsubmit={(e) => {
      e.preventDefault();
      submit();
    }}
  >
    <UnsavedBar dirty={page !== saved} {saving} onsave={submit} onreset={() => (page = saved)} />
    <SettingsSection
      id="maintenance-section"
      title="Maintenance mode"
      description="Show a maintenance page with a 503 status on all domains of this resource. The containers keep running, and the change applies without a redeploy."
    >
      {#if data.is_supported}
        <div class="flex flex-col gap-4">
          <div class="grid gap-4 lg:grid-cols-2">
            <Listbox
              id="isMaintenanceEnabled"
              label="Maintenance mode"
              disabled={!data.can_update}
              options={[
                { value: false, label: 'Off: serve the resource' },
                { value: true, label: 'On: show the maintenance page' },
              ]}
              bind:value={enabled}
              onchange={toggle}
            />
          </div>
          <FormTextarea
            id="maintenancePage"
            rows={10}
            monospace
            disabled={!data.can_update}
            label="Custom maintenance page (HTML)"
            placeholder="Leave empty to use the Coolify default maintenance page."
            helper="One self-contained HTML file (max 1024 KB) with inline CSS and images."
            bind:value={page}
          />
          {#if data.can_update}
            <div class="flex gap-2">
              <button type="button" class="button" onclick={reset}>Reset to default</button>
            </div>
          {/if}
        </div>
      {:else}
        <p class="text-sm text-neutral-500 dark:text-neutral-400">Maintenance mode needs a Traefik or Caddy proxy on a server that is not part of a Docker Swarm.</p>
      {/if}
    </SettingsSection>
  </form>
{/if}
