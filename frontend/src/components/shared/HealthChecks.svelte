<script lang="ts">
  // Port of upstream livewire/project/shared/health-checks.blade.php.
  import Callout from '../Callout.svelte';
  import ConfirmModal from '../ConfirmModal.svelte';
  import FormInput from '../FormInput.svelte';
  import Listbox from '../Listbox.svelte';
  import SettingsSection from '../SettingsSection.svelte';
  import SettingsSubsection from '../SettingsSubsection.svelte';
  import UnsavedBar from '../UnsavedBar.svelte';
  import { ApiError, request } from '../../lib/api';
  import { resPath, type ResourceKind } from '../../lib/sharedApi';
  import { notify } from '../../lib/toast.svelte';

  let { kind, uuid, onchange }: { kind: ResourceKind; uuid: string; onchange?: () => void } = $props();

  let form = $state<Record<string, any> | null>(null);
  let saved = $state('');
  let saving = $state(false);
  let confirming = $state(false);

  const base = $derived(`${resPath(kind, uuid)}/healthcheck`);
  const canUpdate = $derived(form?.can_update ?? false);
  const dirty = $derived(form !== null && JSON.stringify(form) !== saved);

  async function load() {
    const d = await request<any>('GET', base);
    d.health_check_type ??= 'http';
    for (const k of ['health_check_port', 'health_check_command', 'health_check_response_text', 'health_check_return_code', 'health_check_interval', 'health_check_timeout', 'health_check_retries', 'health_check_start_period']) {
      d[k] = d[k] === null || d[k] === undefined ? '' : String(d[k]);
    }
    form = d;
    saved = JSON.stringify(d);
  }

  $effect(() => {
    load();
  });

  async function submit() {
    if (saving) return;
    saving = true;
    try {
      const r = await request<{ message: string }>('POST', base, form);
      notify.success(r.message);
      await load();
      onchange?.();
    } catch (err) {
      notify.failure(err instanceof ApiError ? err.message : String(err));
    } finally {
      saving = false;
    }
  }

  async function toggle() {
    try {
      const r = await request<{ message?: string; info?: string }>('POST', `${base}/toggle`, form);
      if (r.info) notify.info('Info', r.info);
      if (r.message) notify.success(r.message);
      await load();
      onchange?.();
    } catch (err) {
      notify.failure(err instanceof ApiError ? err.message : String(err));
    }
  }
</script>

{#if form}
  <form
    onsubmit={(e) => {
      e.preventDefault();
      submit();
    }}
    class="application-settings-form flex flex-col gap-6"
  >
    <UnsavedBar {dirty} {saving} onsave={submit} onreset={() => (form = JSON.parse(saved))} />
    <SettingsSection id="healthcheck-configuration-section" title="Healthcheck" description="Define how Coolify determines whether this application is ready to receive traffic.">
      {#snippet actions()}
        {#if !form?.health_check_enabled}
          <button type="button" class="button button-highlighted" disabled={!canUpdate} onclick={() => (confirming = true)}>Enable</button>
        {:else}
          <button type="button" class="button" disabled={!canUpdate} onclick={toggle}>Disable</button>
        {/if}
      {/snippet}
      {#if form.custom_healthcheck_found}
        <div class="mb-4">
          <Callout type="warning" title="Custom healthcheck detected">Enabling this configuration replaces the custom healthcheck currently defined by the application.</Callout>
        </div>
      {/if}
      <div class="max-w-xs">
        <Listbox id="healthCheckType" label="Check type" required disabled={!canUpdate} bind:value={form.health_check_type} options={[{ value: 'http', label: 'HTTP request' }, { value: 'cmd', label: 'Container command' }]} />
      </div>
      {#if form.health_check_type === 'http'}
        <SettingsSubsection id="healthcheck-request-section" title="HTTP request" helper="Coolify sends this request from inside the container and evaluates the response.">
          <div class="grid gap-4 md:grid-cols-2 lg:grid-cols-4">
            <Listbox id="healthCheckMethod" label="Method" required disabled={!canUpdate} bind:value={form.health_check_method} options={['GET', 'HEAD', 'POST', 'OPTIONS'].map((m) => ({ value: m, label: m }))} />
            <Listbox id="healthCheckScheme" label="Scheme" required disabled={!canUpdate} bind:value={form.health_check_scheme} options={[{ value: 'http', label: 'HTTP' }, { value: 'https', label: 'HTTPS' }]} />
            <FormInput disabled={!canUpdate} id="healthCheckHost" placeholder="localhost" label="Host" required bind:value={form.health_check_host} />
            <FormInput disabled={!canUpdate} type="number" id="healthCheckPort" helper="Uses the first exposed port when empty." placeholder="80" label="Port" bind:value={form.health_check_port} />
          </div>
          <div class="mt-4 grid gap-4 md:grid-cols-[minmax(0,1fr)_10rem]">
            <FormInput disabled={!canUpdate} id="healthCheckPath" placeholder="/health" label="Path" required bind:value={form.health_check_path} />
            <FormInput disabled={!canUpdate} type="number" id="healthCheckReturnCode" placeholder="200" label="Expected code" required bind:value={form.health_check_return_code} />
          </div>
          <div class="mt-4">
            <FormInput disabled={!canUpdate} id="healthCheckResponseText" placeholder="OK" label="Expected response text" helper="Leave empty when the response body does not need to contain specific text." bind:value={form.health_check_response_text} />
          </div>
        </SettingsSubsection>
      {:else}
        <SettingsSubsection id="healthcheck-command-section" title="Container command" helper="The command must exit with code 0 for the container to be considered healthy.">
          <Callout type="warning" title="Shell operators are not supported">The command runs directly inside the container. Operators such as ;, |, &amp;, $, &gt;, and &lt; are blocked.</Callout>
          <div class="mt-4">
            <FormInput disabled={!canUpdate} id="healthCheckCommand" label="Command" placeholder="pg_isready -U postgres" helper="Use a single executable command without shell expansion." required bind:value={form.health_check_command} />
          </div>
        </SettingsSubsection>
      {/if}
    </SettingsSection>
    <SettingsSection id="healthcheck-timing-section" title="Timing and retries" description="Control how quickly healthchecks start, repeat, and fail.">
      <div class="grid gap-4 sm:grid-cols-2 lg:grid-cols-4">
        <FormInput disabled={!canUpdate} min="1" type="number" id="healthCheckInterval" placeholder="30" label="Interval (seconds)" required bind:value={form.health_check_interval} />
        <FormInput disabled={!canUpdate} min="1" type="number" id="healthCheckTimeout" placeholder="30" label="Timeout (seconds)" required bind:value={form.health_check_timeout} />
        <FormInput disabled={!canUpdate} min="1" type="number" id="healthCheckRetries" placeholder="3" label="Retries" required bind:value={form.health_check_retries} />
        <FormInput disabled={!canUpdate} min="1" type="number" id="healthCheckStartPeriod" placeholder="30" label="Start period (seconds)" required bind:value={form.health_check_start_period} />
      </div>
    </SettingsSection>
  </form>
{/if}

{#if confirming}
  <ConfirmModal
    title="Enable healthcheck?"
    actions={['Enable healthcheck for this resource.']}
    warningMessage="If the healthcheck fails, your application will become inaccessible. Review the <a href='https://coolify.io/docs/knowledge-base/health-checks' target='_blank' class='underline text-white'>Healthchecks guide</a> before continuing."
    confirmWithText={false}
    step2ButtonText="Enable healthcheck"
    onconfirm={async () => {
      confirming = false;
      await toggle();
    }}
    onclose={() => (confirming = false)}
  />
{/if}
