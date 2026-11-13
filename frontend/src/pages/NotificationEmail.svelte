<script lang="ts">
  // Port of upstream livewire/notifications/email.blade.php (App\Livewire\Notifications\Email with
  // App\Livewire\Concerns\ManagesSmtpServers).
  import Callout from '../components/Callout.svelte';
  import FormInput from '../components/FormInput.svelte';
  import Listbox from '../components/Listbox.svelte';
  import Checkbox from '../components/Checkbox.svelte';
  import Modal from '../components/Modal.svelte';
  import Reicon from '../components/Reicon.svelte';
  import UnsavedBar from '../components/UnsavedBar.svelte';
  import SettingsSection from '../components/SettingsSection.svelte';
  import SmtpServerPicker from '../components/SmtpServerPicker.svelte';
  import NotificationLayout from '../components/NotificationLayout.svelte';
  import EventMultiselect from '../components/EventMultiselect.svelte';
  import { ApiError } from '../lib/api';
  import { notificationsUi, type ChannelData, type Values } from '../lib/notificationsApi';
  import { emailProviderLabels, emailProviderOptions, type EmailProvider, type SmtpFieldKeys } from '../lib/smtpServers';
  import { notify } from '../lib/toast.svelte';

  document.title = 'Notifications | coolify-rs';

  let data = $state<ChannelData | null>(null);
  let form = $state<Values>({});
  let saved = $state<Values>({});
  let saving = $state(false);
  let testOpen = $state(false);
  let testAddress = $state('');
  /** `$isNewSmtpServer`: the fields hold a server that is not saved yet. */
  let isNew = $state(false);

  const smtpKeys: SmtpFieldKeys = {
    name: 'smtp_server_name',
    fromName: 'smtp_from_name',
    fromAddress: 'smtp_from_address',
    host: 'smtp_host',
    port: 'smtp_port',
    encryption: 'smtp_encryption',
    username: 'smtp_username',
    password: 'smtp_password',
    timeout: 'smtp_timeout',
    ehlo: 'smtp_ehlo_domain',
  };

  /** `syncData()`: the form of the saved settings, with `emailProvider` as the provider in use. */
  function apply(d: ChannelData) {
    data = d;
    const settings = { ...d.settings, email_provider: d.active_provider ?? 'none' };
    saved = { ...settings };
    form = { ...settings };
    isNew = false;
  }
  notificationsUi.get('email').then((d) => {
    testAddress = d.email ?? '';
    apply(d);
  });

  // The provider list saves on change, so it is not an unsaved field.
  const comparable = (v: Values) => JSON.stringify({ ...v, email_provider: null });
  const dirty = $derived(isNew || comparable(form) !== comparable(saved));
  const canUpdate = $derived(!!data?.can_update);
  const activeProvider = $derived<EmailProvider>(data?.active_provider ?? 'none');
  const provider = $derived<EmailProvider>(form.email_provider ?? 'none');
  const sendTestReason = $derived(!canUpdate ? 'Only admins can send a test email.' : !data?.enabled ? 'Save an email provider first.' : null);

  const ev = (column: string, label: string) => ({ column, label, enabled: !!saved[column] });

  async function run(action: string, onfail?: () => void, extra: Values = {}) {
    saving = true;
    try {
      const d = await notificationsUi.action('email', action, { ...$state.snapshot(form), is_new_smtp_server: isNew }, extra);
      if (d.pending) {
        // Not complete yet: the fields of the provider and the "Not active yet" notice show.
        return;
      }
      if (d.new_server) {
        // `addSmtpServer`: a new server only changes the fields; Save stores it.
        Object.assign(form, d.new_server);
        isNew = true;
        return;
      }
      if (action === 'renameServer') {
        // Only the name is saved; other unsaved changes on the page stay unsaved.
        data = d;
        form.smtp_server_name = saved.smtp_server_name = d.settings.smtp_server_name;
      } else {
        apply(d);
      }
      if (d.message) notify.success(d.message);
    } catch (e) {
      onfail?.();
      notify.failure(e instanceof ApiError ? e.message : String(e));
    } finally {
      saving = false;
    }
  }

  /** `Notifications\Concerns\TogglesNotificationEvents::toggleEvent`. */
  function toggleEvent(column: string) {
    form[column] = !form[column];
    run('save', () => (form[column] = !form[column]));
  }

  /** `cancelNewSmtpServer`: back to the saved server, with the provider that was selected. */
  function cancelNewServer() {
    const selected = form.email_provider;
    form = { ...saved, email_provider: selected };
    isNew = false;
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
            run('submit');
          }}
        >
          <UnsavedBar {dirty} {saving} onsave={() => run('submit')} onreset={cancelNewServer} />
          <SettingsSection title="Email delivery">
            {#snippet actions()}
              {#if sendTestReason === null}
                <button type="button" class="button" onclick={() => (testOpen = true)}>
                  <Reicon name="notifications" class="size-3.5" />
                  Send test
                </button>
              {:else}
                <button type="button" class="button" disabled title={sendTestReason}>
                  <Reicon name="notifications" class="size-3.5" />
                  Send test
                </button>
              {/if}
            {/snippet}
            <div class="grid gap-4 lg:grid-cols-2">
              <div class="lg:col-span-2">
                <div class="w-full sm:w-72">
                  <Listbox
                    id="useInstanceEmailSettings"
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
            </div>
          </SettingsSection>
        </form>

        {#if !form.use_instance_email_settings}
          <div class="application-settings-form">
            <SettingsSection title="Email provider" description="Only one provider sends email. Select a provider to see its settings.">
              {#snippet badges()}
                <div class="w-40">
                  <Listbox id="emailProvider" disabled={!canUpdate} bind:value={form.email_provider} onchange={() => run('provider')} options={emailProviderOptions} />
                </div>
              {/snippet}
              <div class="grid gap-4 lg:grid-cols-3">
                {#if provider === 'none'}
                  <p class="text-sm text-neutral-500 lg:col-span-3 dark:text-neutral-400">
                    The team does not get email notifications. Select a provider next to the title to set it up.
                  </p>
                {/if}

                {#if provider !== activeProvider}
                  <div class="lg:col-span-3">
                    <Callout type="warning" title="Not active yet">
                      Enter the {emailProviderLabels[provider]} settings and save.
                      {#if activeProvider === 'none'}
                        Until then, the team does not get email notifications.
                      {:else}
                        Until then, Coolify sends email with
                        {emailProviderLabels[activeProvider]}.
                      {/if}
                    </Callout>
                  </div>
                {/if}

                {#if provider === 'smtp'}
                  <SmtpServerPicker
                    bind:form
                    keys={smtpKeys}
                    {canUpdate}
                    {isNew}
                    activeName={data.smtp_server_name ?? 'Default'}
                    options={data.smtp_server_options ?? []}
                    presets={data.smtp_presets ?? []}
                    onswitch={(id) => run('switchServer', undefined, { id })}
                    onrename={(name) => run('renameServer', undefined, { name })}
                    ondelete={() => run('deleteServer')}
                    onadd={(name, template) => run('addServer', undefined, { name, template })}
                    oncancel={cancelNewServer}
                  />
                {:else if provider === 'resend'}
                  <div>
                    {#if canUpdate}
                      <FormInput required type="password" id="resendApiKey" placeholder="API key" label="Resend API key" autocomplete="new-password" bind:value={form.resend_api_key} />
                    {:else}
                      <FormInput id="resendApiKey" disabled label="Resend API key" value="Hidden (only admins can view)" />
                    {/if}
                  </div>
                  <FormInput disabled={!canUpdate} required id="resendFromName" helper="Name shown in email from Resend." label="From name" bind:value={form.resend_from_name} />
                  <FormInput
                    disabled={!canUpdate}
                    required
                    id="resendFromAddress"
                    helper="Address used in email from Resend. It must be on a domain verified in Resend."
                    label="From address"
                    bind:value={form.resend_from_address}
                  />
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
                events={[
                  ev('status_change_email_notifications', 'Resource status changes'),
                  ev('restart_limit_reached_email_notifications', 'Restart limit reached'),
                  ev('image_vulnerabilities_email_notifications', 'Image vulnerabilities'),
                ]}
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
                id="deploymentCommitDetailsEmailNotifications"
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
