<script lang="ts">
  // Port of upstream livewire/notifications/email.blade.php.
  import FormInput from '../components/FormInput.svelte';
  import Listbox from '../components/Listbox.svelte';
  import Checkbox from '../components/Checkbox.svelte';
  import Modal from '../components/Modal.svelte';
  import Reicon from '../components/Reicon.svelte';
  import UnsavedBar from '../components/UnsavedBar.svelte';
  import SettingsSection from '../components/SettingsSection.svelte';
  import NotificationLayout from '../components/NotificationLayout.svelte';
  import EventMultiselect from '../components/EventMultiselect.svelte';
  import { ApiError } from '../lib/api';
  import { notificationsUi, type ChannelData, type Values } from '../lib/notificationsApi';
  import { notify } from '../lib/toast.svelte';

  document.title = 'Notifications | coolify-rs';

  let data = $state<ChannelData | null>(null);
  let form = $state<Values>({});
  let saved = $state<Values>({});
  let saving = $state(false);
  let testOpen = $state(false);
  let testAddress = $state('');
  let instanceAdmin = $state(false);

  function apply(d: ChannelData) {
    data = d;
    saved = { ...d.settings };
    form = { ...d.settings };
  }
  notificationsUi.get('email').then((d) => {
    instanceAdmin = !!d.instance_admin;
    testAddress = d.email ?? '';
    apply(d);
  });

  const dirty = $derived(JSON.stringify(form) !== JSON.stringify(saved));
  const canUpdate = $derived(!!data?.can_update);
  const onOff = [
    { value: true, label: 'Enabled' },
    { value: false, label: 'Disabled' },
  ];

  const ev = (column: string, label: string) => ({ column, label, enabled: !!saved[column] });

  async function run(action: string, onfail?: () => void) {
    saving = true;
    try {
      const d = await notificationsUi.action('email', action, $state.snapshot(form));
      apply(d);
      notify.success(d.message ?? '');
    } catch (e) {
      onfail?.();
      notify.failure(e instanceof ApiError ? e.message : String(e));
    } finally {
      saving = false;
    }
  }

  function toggleEvent(column: string) {
    form[column] = !form[column];
    run('save', () => (form[column] = !form[column]));
  }

  async function sendTest(e: SubmitEvent) {
    e.preventDefault();
    testOpen = false;
    try {
      const r = await notificationsUi.test('email', testAddress);
      notify.success(r.message);
    } catch (err) {
      notify.failure(err instanceof ApiError ? err.message : String(err));
    }
  }
</script>

<div>
  <NotificationLayout>
    {#if data}
      <div class="flex flex-col gap-6">
        <form
          class="application-settings-form"
          onsubmit={(e) => {
            e.preventDefault();
            run('save');
          }}
        >
          <UnsavedBar {dirty} {saving} onsave={() => run('save')} onreset={() => (form = { ...saved })} />
          <SettingsSection title="Email delivery">
            {#snippet actions()}
              {#if canUpdate}
                {#if data?.enabled}
                  <button type="button" class="button" onclick={() => (testOpen = true)}>
                    <Reicon name="notifications" class="size-3.5" />
                    Send test
                  </button>
                {:else}
                  <button type="button" class="button" disabled>Send test</button>
                {/if}
              {/if}
            {/snippet}
            <div class="grid gap-4 lg:grid-cols-2">
              <div class="lg:col-span-2">
                <div class="w-full sm:w-72">
                  <Listbox
                    id="use_instance_email_settings"
                    label="Email service"
                    disabled={!canUpdate}
                    bind:value={form.use_instance_email_settings}
                    onchange={(v) => run('instant', () => (form.use_instance_email_settings = !v))}
                    options={[
                      { value: true, label: 'Use system-wide settings' },
                      { value: false, label: 'Use team email settings' },
                    ]}
                  />
                </div>
              </div>
              {#if !form.use_instance_email_settings}
                <FormInput id="smtp_from_name" required disabled={!canUpdate} helper="Name used in emails." label="From name" bind:value={form.smtp_from_name} />
                <FormInput
                  id="smtp_from_address"
                  required
                  disabled={!canUpdate}
                  helper="Email address used in emails."
                  label="From address"
                  bind:value={form.smtp_from_address}
                />
                {#if instanceAdmin}
                  <div class="lg:col-span-2">
                    <button type="button" class="button" onclick={() => run('copy')}>Copy from instance settings</button>
                  </div>
                {/if}
              {/if}
            </div>
          </SettingsSection>
        </form>

        {#if !form.use_instance_email_settings}
          <div class="application-settings-form">
            <SettingsSection title="SMTP server" description="Deliver messages through your own SMTP server.">
              <div class="grid gap-4 lg:grid-cols-3">
                <div class="lg:col-span-3">
                  <div class="w-full sm:w-72">
                    <Listbox
                      id="smtp_enabled"
                      label="SMTP delivery"
                      disabled={!canUpdate}
                      bind:value={form.smtp_enabled}
                      onchange={() => run('smtp', () => (form.smtp_enabled = false))}
                      options={onOff}
                    />
                  </div>
                </div>
                <FormInput id="smtp_host" required disabled={!canUpdate} placeholder="smtp.mailgun.org" label="Host" bind:value={form.smtp_host} />
                <FormInput id="smtp_port" required disabled={!canUpdate} type="number" placeholder="587" label="Port" bind:value={form.smtp_port} />
                <Listbox
                  id="smtp_encryption"
                  label="Encryption"
                  required
                  disabled={!canUpdate}
                  bind:value={form.smtp_encryption}
                  options={[
                    { value: 'starttls', label: 'StartTLS' },
                    { value: 'tls', label: 'TLS / SSL' },
                    { value: 'none', label: 'None' },
                  ]}
                />
                <FormInput id="smtp_username" disabled={!canUpdate} label="SMTP username" bind:value={form.smtp_username} />
                {#if canUpdate}
                  <FormInput id="smtp_password" type="password" label="SMTP password" bind:value={form.smtp_password} />
                {:else}
                  <FormInput id="smtp_password" disabled label="SMTP password" value="Hidden (only admins can view)" />
                {/if}
                <FormInput id="smtp_timeout" type="number" disabled={!canUpdate} helper="Timeout value for sending emails." label="Timeout" bind:value={form.smtp_timeout} />
                <FormInput
                  id="smtp_ehlo_domain"
                  disabled={!canUpdate}
                  placeholder="coolify.example.com"
                  helper="Fully qualified domain sent in the SMTP EHLO command. Uses the system default when empty."
                  label="EHLO domain"
                  bind:value={form.smtp_ehlo_domain}
                />
              </div>
            </SettingsSection>
          </div>

          <div class="application-settings-form">
            <SettingsSection title="Resend">
              <div class="grid gap-4 lg:grid-cols-2">
                <Listbox id="resend_enabled" label="Resend delivery" disabled={!canUpdate} bind:value={form.resend_enabled} onchange={() => run('resend')} options={onOff} />
                {#if canUpdate}
                  <FormInput
                    id="resend_api_key"
                    type="password"
                    required={!!form.resend_enabled}
                    placeholder="API key"
                    label="API key"
                    autocomplete="new-password"
                    bind:value={form.resend_api_key}
                  />
                {:else}
                  <FormInput id="resend_api_key" disabled label="API key" value="Hidden (only admins can view)" />
                {/if}
              </div>
            </SettingsSection>
          </div>
        {/if}

        <div class="application-settings-form">
          <SettingsSection title="Notification events">
            <div class="grid gap-4 lg:grid-cols-2">
              <EventMultiselect
                id="deployment-email-events"
                label="Deployments"
                disabled={!canUpdate}
                ontoggle={toggleEvent}
                events={[ev('deployment_success_email_notifications', 'Deployment success'), ev('deployment_failure_email_notifications', 'Deployment failure')]}
              />
              <EventMultiselect
                id="resource-email-events"
                label="Resources"
                disabled={!canUpdate}
                ontoggle={toggleEvent}
                events={[ev('status_change_email_notifications', 'Resource status changes'), ev('restart_limit_reached_email_notifications', 'Restart limit reached')]}
              />
              <EventMultiselect
                id="backup-email-events"
                label="Backups"
                disabled={!canUpdate}
                ontoggle={toggleEvent}
                events={[ev('backup_success_email_notifications', 'Backup success'), ev('backup_failure_email_notifications', 'Backup failure')]}
              />
              <EventMultiselect
                id="scheduled-task-email-events"
                label="Scheduled tasks"
                disabled={!canUpdate}
                ontoggle={toggleEvent}
                events={[ev('scheduled_task_success_email_notifications', 'Scheduled task success'), ev('scheduled_task_failure_email_notifications', 'Scheduled task failure')]}
              />
              <EventMultiselect
                id="server-email-events"
                label="Server"
                disabled={!canUpdate}
                ontoggle={toggleEvent}
                events={[
                  ev('docker_cleanup_success_email_notifications', 'Docker cleanup success'),
                  ev('docker_cleanup_failure_email_notifications', 'Docker cleanup failure'),
                  ev('server_disk_usage_email_notifications', 'Server disk usage'),
                  ev('server_reachable_email_notifications', 'Server reachable'),
                  ev('server_unreachable_email_notifications', 'Server unreachable'),
                  ev('server_patch_email_notifications', 'Server patching'),
                  ev('traefik_outdated_email_notifications', 'Traefik proxy outdated'),
                ]}
              />
            </div>
            <div class="mt-4">
              <Checkbox
                id="deployment_commit_details_email_notifications"
                label="Include commit details in deployment emails"
                helper="Adds the commit SHA, branch, and commit message to deployment success and failure emails."
                disabled={!canUpdate}
                bind:checked={form.deployment_commit_details_email_notifications}
                onchange={(v) => run('save', () => (form.deployment_commit_details_email_notifications = !v))}
              />
            </div>
          </SettingsSection>
        </div>
      </div>
    {/if}
  </NotificationLayout>
</div>

{#if testOpen}
  <Modal title="Send Test Email" onclose={() => (testOpen = false)}>
    <form onsubmit={sendTest} class="flex w-full flex-col gap-4">
      <FormInput placeholder="test@example.com" id="testEmailAddress" label="Recipient" required bind:value={testAddress} />
      <div class="flex justify-end border-t border-neutral-200 pt-4 dark:border-white/[0.08]">
        <button type="submit" class="button button-highlighted">Send email</button>
      </div>
    </form>
  </Modal>
{/if}
