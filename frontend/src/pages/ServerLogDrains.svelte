<script lang="ts">
  // Port of upstream livewire/server/log-drains.blade.php on the `/servers/{uuid}/log-drains` route.
  import Empty from '../components/Empty.svelte';
  import FormInput from '../components/FormInput.svelte';
  import FormTextarea from '../components/FormTextarea.svelte';
  import Listbox from '../components/Listbox.svelte';
  import ServerFrame from '../components/ServerFrame.svelte';
  import SettingsSection from '../components/SettingsSection.svelte';
  import StatusBadge from '../components/StatusBadge.svelte';
  import UnsavedBar from '../components/UnsavedBar.svelte';
  import { api, ApiError } from '../lib/api';
  import type { ServerFacts } from '../lib/api';
  import { failure } from '../lib/serverApi';
  import { session } from '../lib/session.svelte';
  import { notify } from '../lib/toast.svelte';

  let { uuid }: { uuid: string } = $props();
  type Form = {
    is_logdrain_newrelic_enabled: boolean;
    logdrain_newrelic_license_key: string;
    logdrain_newrelic_base_uri: string;
    is_logdrain_axiom_enabled: boolean;
    logdrain_axiom_dataset_name: string;
    logdrain_axiom_api_key: string;
    is_logdrain_custom_enabled: boolean;
    logdrain_custom_config: string;
    logdrain_custom_config_parser: string;
    is_logdrain_cloudwatch_enabled: boolean;
    logdrain_cloudwatch_region: string;
    logdrain_cloudwatch_group: string;
    logdrain_cloudwatch_stream_prefix: string;
  };
  let form = $state<Form | null>(null);
  let saved = $state<Form | null>(null);
  let saving = $state(false);
  let error = $state<ApiError | null>(null);
  const url = $derived(`/servers/${encodeURIComponent(uuid)}/log-drains`);
  const canEdit = $derived(session.isAdmin);
  const dirty = $derived(!!form && !!saved && JSON.stringify(form) !== JSON.stringify(saved));
  const active = $derived(!!saved && (saved.is_logdrain_newrelic_enabled || saved.is_logdrain_axiom_enabled || saved.is_logdrain_custom_enabled || saved.is_logdrain_cloudwatch_enabled));
  const onoff = [
    { value: false, label: 'Disabled' },
    { value: true, label: 'Enabled' },
  ];

  async function load(f: ServerFacts) {
    if (!f.functional) return;
    const d = await api.get<Record<string, unknown>>(url);
    const next = Object.fromEntries(Object.entries(d).map(([k, v]) => [k, typeof v === 'boolean' ? v : String(v ?? '')])) as unknown as Form;
    form = { ...next };
    saved = { ...next };
  }
  const payload = () => Object.fromEntries(Object.entries(form!).map(([k, v]) => [k, v === '' ? null : v]));

  async function submit() {
    saving = true;
    error = null;
    try {
      await api.patch(url, payload());
      saved = { ...form! };
      notify.success('Settings saved.');
    } catch (e) {
      error = failure(e);
    } finally {
      saving = false;
    }
  }
  /** `toggleLogDrain`: restores the flags when starting the drain fails. */
  async function toggle() {
    error = null;
    const enabled = form!.is_logdrain_newrelic_enabled || form!.is_logdrain_axiom_enabled || form!.is_logdrain_custom_enabled || form!.is_logdrain_cloudwatch_enabled;
    try {
      await api.patch(url, payload());
      saved = { ...form! };
      notify.success(enabled ? 'Log drain service started.' : 'Log drain service stopped.');
    } catch (e) {
      error = failure(e);
      form = { ...saved! };
    }
  }
  const err = (k: string) => error?.field(k);
</script>

<ServerFrame {uuid} active="log-drains" title="Log Drains" onloaded={load}>
  {#snippet children(f)}
    <div class="application-settings-form flex w-full flex-col gap-6">
      {#if !f.functional}
        <SettingsSection title="Log drains" description="Forward container logs from this server to an external destination.">
          <Empty size="sm" title="Server validation required" description="Validate this server before configuring log drains." iconName="notifications" />
        </SettingsSection>
      {:else if form}
        <SettingsSection id="server-log-drains-overview-section" title="Log drains" description="Forward container logs from this server to one external destination.">
          {#snippet actions()}
            <StatusBadge label={active ? 'Active' : 'Not configured'} type={active ? 'success' : 'neutral'} />
          {/snippet}
          <p class="text-sm leading-6 text-neutral-600 dark:text-fg-dim">Only one log drain can be active at a time. Disable the current destination before enabling another provider.</p>
        </SettingsSection>
        <form
          class="contents"
          onsubmit={(e) => {
            e.preventDefault();
            submit();
          }}
        >
          <UnsavedBar {dirty} {saving} onsave={submit} onreset={() => ((form = { ...saved! }), (error = null))} />
          <SettingsSection id="server-new-relic-drain-section" title="New Relic" description="Send logs through the New Relic Log API.">
            <div class="grid gap-4 lg:grid-cols-3">
              <Listbox id="isLogDrainNewRelicEnabled" label="Status" bind:value={form.is_logdrain_newrelic_enabled} onchange={toggle} options={onoff} disabled={form.is_logdrain_axiom_enabled || form.is_logdrain_custom_enabled || form.is_logdrain_cloudwatch_enabled || !canEdit} />
              <FormInput type="password" required={form.is_logdrain_newrelic_enabled} id="logDrainNewRelicLicenseKey" label="License key" disabled={active || !canEdit} bind:value={form.logdrain_newrelic_license_key} error={err('logdrain_newrelic_license_key')} />
              <FormInput required={form.is_logdrain_newrelic_enabled} id="logDrainNewRelicBaseUri" label="Endpoint" placeholder="https://log-api.eu.newrelic.com/log/v1" helper="Use the EU or US New Relic Log API endpoint." disabled={active || !canEdit} bind:value={form.logdrain_newrelic_base_uri} error={err('logdrain_newrelic_base_uri')} />
            </div>
          </SettingsSection>
          <SettingsSection id="server-axiom-drain-section" title="Axiom" description="Send logs to an Axiom dataset using its ingest API.">
            <div class="grid gap-4 lg:grid-cols-3">
              <Listbox id="isLogDrainAxiomEnabled" label="Status" bind:value={form.is_logdrain_axiom_enabled} onchange={toggle} options={onoff} disabled={form.is_logdrain_newrelic_enabled || form.is_logdrain_custom_enabled || form.is_logdrain_cloudwatch_enabled || !canEdit} />
              <FormInput type="password" required={form.is_logdrain_axiom_enabled} id="logDrainAxiomApiKey" label="API key" disabled={active || !canEdit} bind:value={form.logdrain_axiom_api_key} error={err('logdrain_axiom_api_key')} />
              <FormInput required={form.is_logdrain_axiom_enabled} id="logDrainAxiomDatasetName" label="Dataset name" disabled={active || !canEdit} bind:value={form.logdrain_axiom_dataset_name} error={err('logdrain_axiom_dataset_name')} />
            </div>
          </SettingsSection>
          <SettingsSection id="server-cloudwatch-drain-section" title="Amazon CloudWatch Logs" description="Send container logs directly to CloudWatch Logs using the Docker awslogs logging driver.">
            <div class="mb-4 max-w-sm">
              <Listbox id="isLogDrainCloudwatchEnabled" label="Status" bind:value={form.is_logdrain_cloudwatch_enabled} onchange={toggle} options={onoff} disabled={form.is_logdrain_newrelic_enabled || form.is_logdrain_axiom_enabled || form.is_logdrain_custom_enabled || !canEdit} />
            </div>
            <div class="grid gap-4 lg:grid-cols-3">
              <FormInput required={form.is_logdrain_cloudwatch_enabled} id="logDrainCloudwatchGroup" label="Log group" placeholder="/coolify/production" disabled={active || !canEdit} bind:value={form.logdrain_cloudwatch_group} error={err('logdrain_cloudwatch_group')} />
              <FormInput id="logDrainCloudwatchStreamPrefix" label="Log stream prefix" placeholder="docker/" disabled={active || !canEdit} bind:value={form.logdrain_cloudwatch_stream_prefix} error={err('logdrain_cloudwatch_stream_prefix')} />
              <FormInput required={form.is_logdrain_cloudwatch_enabled} id="logDrainCloudwatchRegion" label="Region" placeholder="us-east-1" disabled={active || !canEdit} bind:value={form.logdrain_cloudwatch_region} error={err('logdrain_cloudwatch_region')} />
            </div>
          </SettingsSection>
          <SettingsSection id="server-custom-drain-section" title="Custom Fluent Bit" description="Provide a custom Fluent Bit output and optional parser configuration.">
            <div class="mb-4 max-w-sm">
              <Listbox id="isLogDrainCustomEnabled" label="Status" bind:value={form.is_logdrain_custom_enabled} onchange={toggle} options={onoff} disabled={form.is_logdrain_newrelic_enabled || form.is_logdrain_axiom_enabled || form.is_logdrain_cloudwatch_enabled || !canEdit} />
            </div>
            <div class="grid gap-4 lg:grid-cols-2">
              <FormTextarea rows={8} required={form.is_logdrain_custom_enabled} id="logDrainCustomConfig" label="Fluent Bit configuration" disabled={active || !canEdit} bind:value={form.logdrain_custom_config} error={err('logdrain_custom_config')} />
              <FormTextarea rows={8} id="logDrainCustomConfigParser" label="Parser configuration" disabled={active || !canEdit} bind:value={form.logdrain_custom_config_parser} error={err('logdrain_custom_config_parser')} />
            </div>
          </SettingsSection>
        </form>
      {/if}
    </div>
  {/snippet}
</ServerFrame>
