<script lang="ts">
  // Port of upstream livewire/project/database/health.blade.php (Project/Database/Health).
  import { untrack } from 'svelte';
  import Callout from './Callout.svelte';
  import ConfirmModal from './ConfirmModal.svelte';
  import FormInput from './FormInput.svelte';
  import SettingsSection from './SettingsSection.svelte';
  import UnsavedBar from './UnsavedBar.svelte';
  import { ApiError, type DatabaseRecord } from '../lib/api';
  import { generalUi } from '../lib/databaseApi';
  import { notify } from '../lib/toast.svelte';

  let { record, canUpdate, onsaved }: { record: DatabaseRecord; canUpdate: boolean; onsaved: () => Promise<void> } = $props();

  const read = (r: DatabaseRecord) => ({
    interval: String(r.health_check_interval ?? 15),
    timeout: String(r.health_check_timeout ?? 5),
    retries: String(r.health_check_retries ?? 5),
    start_period: String(r.health_check_start_period ?? 5),
  });
  let form = $state(untrack(() => read(record)));
  let saved = $state(untrack(() => read(record)));
  let enabled = $state(untrack(() => !!record.health_check_enabled));
  let saving = $state(false);
  let confirming = $state(false);
  let error = $state<ApiError | null>(null);
  const dirty = $derived(JSON.stringify(form) !== JSON.stringify(saved));

  const body = (toggle: boolean) => ({
    health_check_enabled: enabled,
    toggle,
    health_check_interval: form.interval,
    health_check_timeout: form.timeout,
    health_check_retries: form.retries,
    health_check_start_period: form.start_period,
  });

  async function send(toggle: boolean) {
    saving = true;
    error = null;
    try {
      const r = await generalUi.health(record.uuid, body(toggle));
      if (r.health_check_enabled !== undefined) enabled = r.health_check_enabled;
      saved = { ...form };
      await onsaved();
    } catch (e) {
      error = e instanceof ApiError ? e : new ApiError(0, String(e));
      if (!Object.keys(error.errors).length) notify.failure(error.message);
    } finally {
      saving = false;
    }
  }
</script>

<form
  class="application-settings-form flex flex-col gap-6"
  onsubmit={(e) => {
    e.preventDefault();
    send(false);
  }}
>
  <UnsavedBar {dirty} {saving} onsave={() => send(false)} onreset={() => ((form = { ...saved }), (error = null))} />

  <SettingsSection title="Healthcheck" description="Define how Docker checks this database and reports its health.">
    {#snippet actions()}
      {#if !enabled}
        <button type="button" class="button button-highlighted" disabled={!canUpdate} onclick={() => (confirming = true)}>Enable healthcheck</button>
      {:else}
        <button type="button" class="button" disabled={!canUpdate} onclick={() => send(true)}>Disable healthcheck</button>
      {/if}
    {/snippet}

    {#if !enabled}
      <Callout type="warning" title="Healthcheck disabled">
        Docker is not running a healthcheck probe, so Coolify cannot report a healthy or unhealthy state.
      </Callout>
    {/if}

    <div class="{!enabled ? 'mt-4 ' : ''}grid gap-4 sm:grid-cols-2 xl:grid-cols-4">
      <FormInput disabled={!canUpdate} min="1" type="number" id="healthCheckInterval" placeholder="15" label="Interval (seconds)" required bind:value={form.interval} error={error?.field('health_check_interval')} />
      <FormInput disabled={!canUpdate} min="1" type="number" id="healthCheckTimeout" placeholder="5" label="Timeout (seconds)" required bind:value={form.timeout} error={error?.field('health_check_timeout')} />
      <FormInput disabled={!canUpdate} min="1" type="number" id="healthCheckRetries" placeholder="5" label="Retries" required bind:value={form.retries} error={error?.field('health_check_retries')} />
      <FormInput disabled={!canUpdate} min="0" type="number" id="healthCheckStartPeriod" placeholder="5" label="Start period (seconds)" required bind:value={form.start_period} error={error?.field('health_check_start_period')} />
    </div>
  </SettingsSection>
</form>

{#if confirming}
  <ConfirmModal
    title="Enable healthcheck?"
    actions={['Enable healthcheck for this database.']}
    warningMessage="A failing probe marks the database unhealthy. Review the healthcheck guide before enabling it."
    step2ButtonText="Enable healthcheck"
    confirmWithText={false}
    confirmWithPassword={false}
    onconfirm={async () => {
      await send(true);
    }}
    onclose={() => (confirming = false)}
  />
{/if}
