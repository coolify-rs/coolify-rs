<script lang="ts">
  // Port of upstream livewire/project/shared/maintenance.blade.php (+ Shared\Maintenance).
  import ConfirmModal from '../ConfirmModal.svelte';
  import FormTextarea from '../FormTextarea.svelte';
  import StatusBadge from '../StatusBadge.svelte';
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
  let confirming = $state(false);

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

  // `toggleMaintenance` (v4.7.5): the header button calls this after the user confirms.
  async function toggle() {
    try {
      const r = await request<{ message: string }>('POST', `${base}/toggle`, { enabled: !enabled });
      enabled = !enabled;
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

{#snippet badges()}
  <!-- The inline wrapper puts the badge text on the same line as the title text. -->
  <span>
    {#if enabled}
      <StatusBadge label="On" type="success" />
    {:else}
      <StatusBadge label="Off" type="neutral" />
    {/if}
  </span>
{/snippet}

{#snippet actions()}
  <button type="button" class="button {enabled ? '' : 'button-error'}" onclick={() => (confirming = true)}>
    {enabled ? 'Disable maintenance' : 'Enable maintenance'}
  </button>
{/snippet}

{#if confirming}
  <ConfirmModal
    title={enabled ? 'Disable maintenance mode?' : 'Enable maintenance mode?'}
    actions={enabled
      ? ['All domains of this resource serve the resource again.']
      : ['All domains of this resource show the maintenance page with a 503 status.', 'The containers keep running.']}
    warningMessage={enabled ? 'Visitors can reach this resource again right away.' : 'Visitors cannot reach this resource until you disable maintenance mode.'}
    confirmWithText={false}
    confirmWithPassword={false}
    step2ButtonText={enabled ? 'Disable maintenance' : 'Enable maintenance'}
    onconfirm={toggle}
    onclose={() => (confirming = false)}
  />
{/if}

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
      badges={data.is_supported ? badges : undefined}
      actions={data.is_supported && data.can_update ? actions : undefined}
    >
      {#if data.is_supported}
        <div class="flex flex-col gap-4">
          <FormTextarea
            id="maintenancePage"
            rows={10}
            monospace
            disabled={!data.can_update}
            label="Custom maintenance page (HTML)"
            placeholder="Leave empty to use the coolify-rs default maintenance page."
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
