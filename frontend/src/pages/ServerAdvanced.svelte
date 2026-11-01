<script lang="ts">
  // Port of upstream livewire/server/advanced.blade.php.
  import FormInput from '../components/FormInput.svelte';
  import Listbox from '../components/Listbox.svelte';
  import ServerFrame from '../components/ServerFrame.svelte';
  import SettingsSection from '../components/SettingsSection.svelte';
  import UnsavedBar from '../components/UnsavedBar.svelte';
  import { ApiError } from '../lib/api';
  import { get, post, failure } from '../lib/serverApi';
  import { session } from '../lib/session.svelte';
  import { notify } from '../lib/toast.svelte';

  let { uuid }: { uuid: string } = $props();

  type Form = {
    server_disk_usage_check_frequency: string;
    server_disk_usage_notification_threshold: string;
    server_disk_usage_notification_interval_hours: string;
    concurrent_builds: string;
    dynamic_timeout: string;
    deployment_queue_limit: string;
    backup_compression_cpu_percentage: string;
  };

  let form = $state<Form | null>(null);
  let saved = $state<Form | null>(null);
  let saving = $state(false);
  let error = $state<ApiError | null>(null);
  const url = $derived(`/server-page/${encodeURIComponent(uuid)}/advanced`);
  const canEdit = $derived(session.isAdmin);
  const dirty = $derived(!!form && !!saved && JSON.stringify(form) !== JSON.stringify(saved));

  async function load() {
    const d = await get<Record<string, string | number>>(url);
    const f = Object.fromEntries(Object.entries(d).map(([k, v]) => [k, String(v ?? '')])) as Form;
    saved = { ...f };
    form = { ...f };
  }

  async function save(instant = false) {
    if (!form) return;
    saving = true;
    error = null;
    try {
      await post(url, instant ? { ...form, instant: true } : form);
      notify.success('Server updated.');
      saved = { ...form };
    } catch (e) {
      error = failure(e);
    } finally {
      saving = false;
    }
  }
  const err = (k: string) => error?.field(k);
</script>

<ServerFrame {uuid} active="advanced" title="Advanced" onloaded={load}>
  {#snippet children()}
    {#if form}
      <form
        onsubmit={(e) => {
          e.preventDefault();
          save();
        }}
        class="application-settings-form flex w-full flex-col gap-6"
      >
        <UnsavedBar {dirty} {saving} onsave={() => save()} onreset={() => ((form = { ...saved! }), (error = null))} />

        <SettingsSection id="server-disk-usage-section" title="Disk usage" description="Control when coolify-rs checks this server and when your team is notified.">
          <div class="grid gap-4 lg:grid-cols-3">
            <FormInput
              disabled={!canEdit}
              placeholder="0 23 * * *"
              id="serverDiskUsageCheckFrequency"
              label="Check frequency"
              required
              helper="Cron expression or preset such as hourly, daily, weekly, monthly, or yearly."
              bind:value={form.server_disk_usage_check_frequency}
              error={err('server_disk_usage_check_frequency')}
            />
            <FormInput
              disabled={!canEdit}
              id="serverDiskUsageNotificationThreshold"
              type="number"
              min="1"
              max="99"
              label="Notification threshold"
              required
              helper="Notify the team when root filesystem usage exceeds this percentage."
              bind:value={form.server_disk_usage_notification_threshold}
              error={err('server_disk_usage_notification_threshold')}
            />
            <FormInput
              disabled={!canEdit}
              id="serverDiskUsageNotificationIntervalHours"
              type="number"
              min="1"
              max="720"
              label="Notification interval (hours)"
              required
              helper="Minimum time between high disk usage notifications for this server."
              bind:value={form.server_disk_usage_notification_interval_hours}
              error={err('server_disk_usage_notification_interval_hours')}
            />
          </div>
        </SettingsSection>

        <SettingsSection id="server-backups-section" title="Backups" description="Limit how much CPU volume backup compression may use on this server.">
          <Listbox
            disabled={!canEdit}
            id="backupCompressionCpuPercentage"
            label="Backup compression CPU"
            helper="Sets how many CPU threads can be used to compress volume backups, based on this server's available CPUs."
            bind:value={form.backup_compression_cpu_percentage}
            onchange={() => save(true)}
            options={[
              { value: '25', label: 'Low (25%)' },
              { value: '50', label: 'Balanced (50%)' },
              { value: '75', label: 'High (75%)' },
              { value: '100', label: 'Maximum (100%)' },
            ]}
          />
        </SettingsSection>

        <SettingsSection id="server-builds-section" title="Builds" description="Set deployment concurrency, execution timeouts, and queue capacity.">
          <div class="grid gap-4 lg:grid-cols-3">
            <FormInput disabled={!canEdit} id="concurrentBuilds" type="number" min="1" label="Concurrent builds" required helper="Maximum deployments that can build at the same time." bind:value={form.concurrent_builds} error={err('concurrent_builds')} />
            <FormInput disabled={!canEdit} id="dynamicTimeout" type="number" min="1" label="Deployment timeout" required helper="Maximum deployment duration in seconds." bind:value={form.dynamic_timeout} error={err('dynamic_timeout')} />
            <FormInput disabled={!canEdit} id="deploymentQueueLimit" type="number" min="1" label="Queue limit" required helper="Maximum queued deployments before new requests are rejected." bind:value={form.deployment_queue_limit} error={err('deployment_queue_limit')} />
          </div>
        </SettingsSection>
      </form>
    {/if}
  {/snippet}
</ServerFrame>
