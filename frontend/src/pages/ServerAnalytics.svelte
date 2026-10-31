<script lang="ts">
  // Port of upstream livewire/server/analytics/show.blade.php with the settings
  // of traffic-analytics-settings.blade.php and `livewire:analytics` below them.
  import Analytics from '../components/analytics/Analytics.svelte';
  import Callout from '../components/Callout.svelte';
  import Empty from '../components/Empty.svelte';
  import FormInput from '../components/FormInput.svelte';
  import Listbox from '../components/Listbox.svelte';
  import ServerFrame from '../components/ServerFrame.svelte';
  import SettingsSection from '../components/SettingsSection.svelte';
  import UnsavedBar from '../components/UnsavedBar.svelte';
  import ConfirmModal from '../components/ConfirmModal.svelte';
  import { ApiError } from '../lib/api';
  import { get, post, failure } from '../lib/serverApi';
  import { session } from '../lib/session.svelte';
  import { notify } from '../lib/toast.svelte';

  let { uuid }: { uuid: string } = $props();
  type Form = {
    traffic_topn: string;
    traffic_sample_threshold: string;
    traffic_retention_1h_days: string;
    traffic_retention_1d_days: string;
    is_geoip_enabled: boolean;
    geoip_refresh_days: string;
    geoip_maxmind_license_key: string;
    traffic_ip_mode: string;
  };
  let enabled = $state(false);
  let loaded = $state(false);
  /** `trafficAnalyticsStateChanged`. */
  let stateVersion = $state(0);
  let reason = $state<string | null>(null);
  let caddyNote = $state<string | null>(null);
  let form = $state<Form | null>(null);
  let saved = $state<Form | null>(null);
  let saving = $state(false);
  let error = $state<ApiError | null>(null);
  const url = $derived(`/server-page/${encodeURIComponent(uuid)}/analytics`);
  const canEdit = $derived(session.isAdmin);
  const dirty = $derived(!!form && !!saved && JSON.stringify(form) !== JSON.stringify(saved));

  async function load() {
    const d = await get<Record<string, unknown>>(url);
    enabled = d.enabled as boolean;
    loaded = true;
    reason = d.unsupported_reason as string | null;
    caddyNote = d.caddy_note as string | null;
    const f: Form = {
      traffic_topn: String(d.traffic_topn ?? ''),
      traffic_sample_threshold: String(d.traffic_sample_threshold ?? ''),
      traffic_retention_1h_days: String(d.traffic_retention_1h_days ?? ''),
      traffic_retention_1d_days: String(d.traffic_retention_1d_days ?? ''),
      is_geoip_enabled: d.is_geoip_enabled as boolean,
      geoip_refresh_days: String(d.geoip_refresh_days ?? ''),
      geoip_maxmind_license_key: (d.geoip_maxmind_license_key as string) ?? '',
      traffic_ip_mode: d.traffic_ip_mode as string,
    };
    form = { ...f };
    saved = { ...f };
  }
  let toggling = $state(false);
  let confirmDisable = $state(false);
  let confirmEnable = $state(false);
  /** `toggleTrafficAnalytics`. */
  async function toggle() {
    toggling = true;
    try {
      const r = await post<{ message: string }>(`${url}/toggle`);
      notify.success(r.message);
      await load();
      stateVersion++;
    } catch (e) {
      failure(e);
    } finally {
      toggling = false;
    }
  }
  async function save() {
    if (!form) return;
    saving = true;
    error = null;
    try {
      await post(url, form);
      saved = { ...form };
      notify.success('Traffic analytics settings updated. Restarting Sentinel.');
    } catch (e) {
      error = failure(e);
    } finally {
      saving = false;
    }
  }
  const err = (k: string) => error?.field(k);
</script>

<ServerFrame {uuid} active="analytics" title="Analytics" onloaded={load}>
  {#snippet children()}
    <div class="flex w-full min-w-0 flex-col gap-6">
      {#if canEdit && form}
        <div class="application-settings-form flex w-full flex-col gap-6">
          <form
            class="contents"
            onsubmit={(e) => {
              e.preventDefault();
              save();
            }}
          >
            {#if enabled}
              <UnsavedBar {dirty} {saving} onsave={save} onreset={() => ((form = { ...saved! }), (error = null))} />
            {/if}
            <SettingsSection id="server-traffic-analytics-settings-section" title="Traffic analytics" description="Control proxy traffic collection, retention, and visitor geolocation for this server.">
              {#snippet actions()}
                {#if enabled}
                  <div class="flex items-center gap-3">
                    <button type="button" class="button" disabled={!canEdit} onclick={() => (confirmDisable = true)}>Disable traffic analytics</button>
                  </div>
                {/if}
              {/snippet}
              {#if enabled}
                {#if caddyNote}
                  <Callout type="info" title="Redeploy to start logging">{caddyNote}</Callout>
                {/if}
                <div class="grid gap-4 lg:grid-cols-2">
                  <FormInput disabled={!canEdit} type="number" min="1" id="trafficTopn" label="Top-N cap" required helper="Maximum distinct values kept per dimension (paths, countries, browsers). Overflow folds into Other." bind:value={form.traffic_topn} error={err('traffic_topn')} />
                  <FormInput disabled={!canEdit} type="number" min="0" id="trafficSampleThreshold" label="Sample threshold" required helper="Events per second above which Sentinel starts sampling. 0 disables sampling." bind:value={form.traffic_sample_threshold} error={err('traffic_sample_threshold')} />
                  <FormInput disabled={!canEdit} type="number" min="1" id="trafficRetention1hDays" label="Hourly retention" required helper="Days of hourly rollups to keep. This is the fine-grained history window." bind:value={form.traffic_retention_1h_days} error={err('traffic_retention_1h_days')} />
                  <FormInput disabled={!canEdit} type="number" min="1" id="trafficRetention1dDays" label="Daily retention" required helper="Days of daily rollups to keep before deletion." bind:value={form.traffic_retention_1d_days} error={err('traffic_retention_1d_days')} />
                  <Listbox disabled={!canEdit} id="isGeoipEnabled" label="Geolocation" helper="Country enrichment from visitor IPs. Disable to skip GeoIP lookups." bind:value={form.is_geoip_enabled} options={[{ value: true, label: 'Enabled' }, { value: false, label: 'Disabled' }]} />
                  <Listbox disabled={!canEdit} id="trafficIpMode" label="Client IP addresses" helper="How Sentinel stores visitor IPs for the Top IPs list. Anonymized keeps only the network. Disabled does not store IPs or visitor cities." bind:value={form.traffic_ip_mode} options={[{ value: 'full', label: 'Full' }, { value: 'anonymized', label: 'Anonymized (/24 IPv4, /48 IPv6)' }, { value: 'off', label: 'Disabled' }]} />
                  <FormInput disabled={!canEdit} type="number" min="1" id="geoipRefreshDays" label="GeoIP refresh interval" required helper="Days between GeoIP database update checks." bind:value={form.geoip_refresh_days} error={err('geoip_refresh_days')} />
                  <FormInput disabled={!canEdit} type="password" id="geoipMaxmindLicenseKey" label="MaxMind GeoIP license key" placeholder="Optional" helper="Used to download the GeoLite2 database for visitor geolocation. Leave empty to use the default database source." bind:value={form.geoip_maxmind_license_key} error={err('geoip_maxmind_license_key')} />
                </div>
              {:else}
                <Empty size="sm" title="Traffic analytics is disabled" description={reason ?? 'Enable traffic analytics to collect proxy access logs and geolocate visitor traffic.'} iconName="dashboard">
                  {#snippet contents()}
                    <div class="flex items-center gap-3">
                      <button type="button" class="button" {...{ ishighlighted: '' }} disabled={!canEdit || !!reason || toggling} title={reason ?? undefined} onclick={() => (confirmEnable = true)}>Enable traffic analytics</button>
                    </div>
                  {/snippet}
                </Empty>
              {/if}
            </SettingsSection>
          </form>
        </div>
      {/if}
      {#if loaded}
        <Analytics scopedServerUuid={uuid} {stateVersion} hideSkeleton={!enabled} />
      {/if}
    </div>
  {/snippet}
</ServerFrame>

{#if confirmEnable}
  <ConfirmModal
    title="Enable traffic analytics?"
    actions={['Enabling traffic analytics will restart Sentinel and the proxy. Your applications will experience a brief interruption.']}
    warningMessage="Application traffic may be interrupted while Sentinel and the proxy restart."
    confirmWithText={false}
    step2ButtonText="Enable traffic analytics"
    onconfirm={toggle}
    onclose={() => (confirmEnable = false)}
  />
{/if}

{#if confirmDisable}
  <ConfirmModal
    title="Disable traffic analytics?"
    actions={['Disabling traffic analytics will restart Sentinel and the proxy. Your applications will experience a brief interruption.']}
    warningMessage="Application traffic may be interrupted while Sentinel and the proxy restart."
    confirmWithText={false}
    step2ButtonText="Disable traffic analytics"
    onconfirm={toggle}
    onclose={() => (confirmDisable = false)}
  />
{/if}
