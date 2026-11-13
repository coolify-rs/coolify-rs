<script lang="ts">
  // Port of upstream livewire/settings-email.blade.php (App\Livewire\SettingsEmail with
  // App\Livewire\Concerns\ManagesSmtpServers) and livewire/partials/settings-email-send-test.blade.php.
  import Callout from '../components/Callout.svelte';
  import Checkbox from '../components/Checkbox.svelte';
  import FormInput from '../components/FormInput.svelte';
  import Listbox from '../components/Listbox.svelte';
  import Modal from '../components/Modal.svelte';
  import Reicon from '../components/Reicon.svelte';
  import SettingsLayout from '../components/SettingsLayout.svelte';
  import SettingsSection from '../components/SettingsSection.svelte';
  import SmtpServerPicker from '../components/SmtpServerPicker.svelte';
  import UnsavedBar from '../components/UnsavedBar.svelte';
  import { ApiError } from '../lib/api';
  import { router } from '../lib/router.svelte';
  import { settingsUi, type EmailSettingsResult, type Values } from '../lib/settingsApi';
  import { emailProviderLabels, emailProviderOptions, type EmailProvider, type SmtpFieldKeys, type SmtpPreset, type SmtpServerOption } from '../lib/smtpServers';
  import { notify } from '../lib/toast.svelte';

  document.title = 'Transactional Email | coolify-rs';

  let form = $state<Values | null>(null);
  let saved = $state<Values>({});
  let canSendTest = $state(false);
  let transactionalEnabled = $state(false);
  let activeProvider = $state<EmailProvider>('none');
  let serverName = $state('Default');
  let serverOptions = $state<SmtpServerOption[]>([]);
  let presets = $state<SmtpPreset[]>([]);
  /** `$isNewSmtpServer`: the fields hold a server that is not saved yet. */
  let isNew = $state(false);
  let testOpen = $state(false);
  let testAddress = $state('');
  let testUsesNotificationSender = $state(false);
  let saving = $state(false);

  const smtpKeys: SmtpFieldKeys = {
    name: 'smtpServerName',
    fromName: 'smtpFromName',
    fromAddress: 'smtpFromAddress',
    host: 'smtpHost',
    port: 'smtpPort',
    encryption: 'smtpEncryption',
    username: 'smtpUsername',
    password: 'smtpPassword',
    timeout: 'smtpTimeout',
    ehlo: 'smtpEhloDomain',
  };

  // One bar for the whole page. Save validates only the provider that is selected.
  const targets = [
    'smtpFromName',
    'smtpFromAddress',
    'smtpHost',
    'smtpPort',
    'smtpEncryption',
    'smtpUsername',
    'smtpPassword',
    'smtpTimeout',
    'smtpEhloDomain',
    'resendApiKey',
    'resendFromName',
    'resendFromAddress',
    'notificationFromName',
    'notificationFromAddress',
  ];
  const dirty = $derived(!!form && (isNew || targets.some((k) => (form![k] ?? '') !== (saved[k] ?? ''))));
  const provider = $derived<EmailProvider>(form?.emailProvider ?? 'none');

  settingsUi.email().then((d) => {
    if (d.redirect) return router.go(d.redirect, true);
    testAddress = d.test_email ?? '';
    page(d);
    apply(d.settings!);
  });

  /** What the page shows next to the form: the provider in use and the SMTP servers. */
  function page(d: EmailSettingsResult) {
    if (d.can_send_test !== undefined) canSendTest = d.can_send_test;
    if (d.transactional_enabled !== undefined) transactionalEnabled = d.transactional_enabled;
    if (d.active_provider) activeProvider = d.active_provider;
    if (d.smtp_server_name !== undefined) serverName = d.smtp_server_name;
    if (d.smtp_server_options) serverOptions = d.smtp_server_options;
    if (d.smtp_presets) presets = d.smtp_presets;
  }

  /** `syncData()`. */
  function apply(s: Values) {
    saved = { ...s };
    form = { ...s };
    isNew = false;
  }

  function outcome(action: string, r: EmailSettingsResult) {
    if (r.pending) {
      // Not complete yet: the fields of the provider and the "Not active yet" notice show.
      return;
    }
    page(r);
    if (r.new_server && form) {
      // `addSmtpServer`: a new server only changes the fields; Save stores it.
      Object.assign(form, r.new_server);
      isNew = true;
    } else if (action === 'renameServer' && r.settings && form) {
      // Only the name is saved; other unsaved changes on the page stay unsaved.
      form.smtpServerName = saved.smtpServerName = r.settings.smtpServerName;
    } else if (action === 'test' && r.settings && form) {
      // The senders on the page were saved; the other fields stay as they are.
      for (const k of ['smtpFromName', 'smtpFromAddress', 'resendFromName', 'resendFromAddress', 'notificationFromName', 'notificationFromAddress']) {
        saved[k] = r.settings[k];
      }
    } else if (r.settings) {
      apply(r.settings);
    }
    if (r.error) notify.failure(r.error);
    if (r.message) notify.success(r.message);
  }

  async function run(action: string, extra: Values = {}) {
    if (!form) return;
    saving = true;
    try {
      outcome(action, await settingsUi.emailAction(action, { ...$state.snapshot(form), isNewSmtpServer: isNew, ...extra }));
    } catch (e) {
      notify.failure(e instanceof ApiError ? e.message : String(e));
    } finally {
      saving = false;
    }
  }

  /** `cancelNewSmtpServer`: back to the saved server, with the provider that was selected. */
  function cancelNewServer() {
    if (!form) return;
    const selected = form.emailProvider;
    form = { ...saved, emailProvider: selected };
    isNew = false;
  }

  function sendTest(e: SubmitEvent) {
    e.preventDefault();
    testOpen = false;
    run('test', { testEmailAddress: testAddress, testUsesNotificationSender });
  }
</script>

<div>
  <SettingsLayout>
    {#if form}
      <div class="application-settings-form mx-auto flex w-full max-w-none min-w-0 flex-col gap-6">
        <UnsavedBar {dirty} {saving} onsave={() => run('submit')} onreset={cancelNewServer} />

        <form
          onsubmit={(e) => {
            e.preventDefault();
            run('submit');
          }}
        >
          <SettingsSection title="Email delivery" description="Only one provider sends email. Select a provider to see its settings.">
            {#snippet badges()}
              <div class="w-40">
                <Listbox id="emailProvider" bind:value={form!.emailProvider} onchange={() => run('provider')} options={emailProviderOptions} />
              </div>
            {/snippet}
            {#snippet actions()}
              {#if canSendTest}
                <button type="button" class="button" onclick={() => (testOpen = true)}>
                  <Reicon name="notifications" class="size-3.5" />
                  Send test
                </button>
              {:else}
                <!-- Always show the button, so that admins know a test is possible once a provider is saved. -->
                <button type="button" class="button" disabled title={transactionalEnabled ? 'Only admins can send a test email.' : 'Save an email provider first.'}>
                  <Reicon name="notifications" class="size-3.5" />
                  Send test
                </button>
              {/if}
            {/snippet}
            <div class="grid gap-4 lg:grid-cols-3">
              {#if provider === 'none'}
                <p class="text-sm text-neutral-500 lg:col-span-3 dark:text-neutral-400">Coolify does not send email. Select a provider next to the title to set it up.</p>
              {/if}

              {#if provider !== activeProvider}
                <div class="lg:col-span-3">
                  <Callout type="warning" title="Not active yet">
                    Enter the {emailProviderLabels[provider]} settings and save.
                    {#if activeProvider === 'none'}
                      Until then, Coolify does not send email.
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
                  canUpdate={true}
                  {isNew}
                  activeName={serverName}
                  options={serverOptions}
                  {presets}
                  onswitch={(id) => run('switchServer', { selectedSmtpServer: id })}
                  onrename={(name) => run('renameServer', { renamedSmtpServerName: name })}
                  ondelete={() => run('deleteServer')}
                  onadd={(name, template) => run('addServer', { newSmtpServerName: name, newSmtpServerTemplate: template })}
                  oncancel={cancelNewServer}
                />
              {:else if provider === 'resend'}
                <FormInput required type="password" id="resendApiKey" placeholder="API key" label="Resend API key" autocomplete="new-password" bind:value={form.resendApiKey} />
                <FormInput required id="resendFromName" helper="Name shown in email from Resend." label="From name" bind:value={form.resendFromName} />
                <FormInput
                  required
                  id="resendFromAddress"
                  helper="Address used in email from Resend. It must be on a domain verified in Resend."
                  label="From address"
                  bind:value={form.resendFromAddress}
                />
              {/if}

              {#if provider !== 'none'}
                <FormInput
                  id="notificationFromName"
                  placeholder="Uses the from name when empty"
                  helper="Name shown in notification emails (deployments, backups, server alerts) to teams that use the instance email settings."
                  label="Notification from name"
                  bind:value={form.notificationFromName}
                />
                <FormInput
                  id="notificationFromAddress"
                  placeholder="Uses the from address when empty"
                  helper="Address used for notification emails. Password resets and invitations still use the from address. Use a separate subdomain (for example alerts@notifications.example.com) to keep the reputation of the two senders apart. Your SMTP provider or Resend must allow this address."
                  label="Notification from address"
                  bind:value={form.notificationFromAddress}
                />
              {/if}
            </div>
          </SettingsSection>
        </form>
      </div>
    {/if}
  </SettingsLayout>
</div>

{#if testOpen}
  <Modal title="Send Test Email" onclose={() => (testOpen = false)}>
    <form onsubmit={sendTest} class="application-settings-form flex flex-col gap-4">
      <FormInput placeholder="test@example.com" id="testEmailAddress" label="Recipient" required bind:value={testAddress} />
      <Checkbox id="testUsesNotificationSender" label="Send from the notification sender" bind:checked={testUsesNotificationSender} />
      <div class="flex justify-end border-t border-neutral-200 pt-4 dark:border-white/[0.08]">
        <button type="submit" class="button">Send email</button>
      </div>
    </form>
  </Modal>
{/if}
