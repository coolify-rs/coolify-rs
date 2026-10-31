<script lang="ts">
  // Port of upstream livewire/settings-email.blade.php (App\Livewire\SettingsEmail) and
  // livewire/partials/settings-email-send-test.blade.php.
  import FormInput from '../components/FormInput.svelte';
  import Listbox from '../components/Listbox.svelte';
  import Modal from '../components/Modal.svelte';
  import Reicon from '../components/Reicon.svelte';
  import SettingsLayout from '../components/SettingsLayout.svelte';
  import SettingsSection from '../components/SettingsSection.svelte';
  import UnsavedBar from '../components/UnsavedBar.svelte';
  import { ApiError } from '../lib/api';
  import { router } from '../lib/router.svelte';
  import { settingsUi, type ActionResult, type Values } from '../lib/settingsApi';
  import { notify } from '../lib/toast.svelte';

  document.title = 'Transactional Email | coolify-rs';

  let form = $state<Values | null>(null);
  let saved = $state<Values>({});
  let canSendTest = $state(false);
  let testOpen = $state(false);
  let testAddress = $state('');
  let saving = $state(false);

  const targets = ['smtpFromName', 'smtpFromAddress', 'smtpHost', 'smtpPort', 'smtpEncryption', 'smtpUsername', 'smtpPassword', 'smtpTimeout', 'smtpEhloDomain', 'resendApiKey'];
  const dirty = $derived(!!form && targets.some((k) => (form![k] ?? '') !== (saved[k] ?? '')));
  const onOff = [
    { value: true, label: 'Enabled' },
    { value: false, label: 'Disabled' },
  ];

  settingsUi.email().then((d) => {
    if (d.redirect) return router.go(d.redirect, true);
    canSendTest = !!d.can_send_test;
    testAddress = d.test_email ?? '';
    apply(d.settings!);
  });

  function apply(s: Values) {
    saved = { ...s };
    form = { ...s };
  }

  function outcome(r: ActionResult) {
    if (r.settings) apply(r.settings);
    if (r.overrides && form) Object.assign(form, r.overrides);
    if (r.can_send_test !== undefined) canSendTest = r.can_send_test;
    if (r.error) notify.failure(r.error);
    if (r.message) notify.success(r.message);
  }

  async function run(action: string, extra: Values = {}) {
    if (!form) return;
    saving = true;
    try {
      outcome(await settingsUi.emailAction(action, { ...$state.snapshot(form), ...extra }));
    } catch (e) {
      notify.failure(e instanceof ApiError ? e.message : String(e));
    } finally {
      saving = false;
    }
  }

  function sendTest(e: SubmitEvent) {
    e.preventDefault();
    testOpen = false;
    run('test', { testEmailAddress: testAddress });
  }
</script>

<div>
  <SettingsLayout>
    {#if form}
      <div class="application-settings-form mx-auto flex w-full max-w-none min-w-0 flex-col gap-6">
        <UnsavedBar {dirty} {saving} onsave={() => run('submit')} onreset={() => form && (form = { ...saved })} />

        <form
          onsubmit={(e) => {
            e.preventDefault();
            run('submit');
          }}
        >
          <SettingsSection title="Sender">
            {#snippet actions()}
              {#if canSendTest}
                <button type="button" class="button" onclick={() => (testOpen = true)}>
                  <Reicon name="notifications" class="size-3.5" />
                  Send test
                </button>
              {/if}
            {/snippet}
            <div class="grid gap-4 lg:grid-cols-2">
              <FormInput required id="smtpFromName" helper="Name shown in outgoing email." label="From name" bind:value={form.smtpFromName} />
              <FormInput required id="smtpFromAddress" helper="Address used for outgoing email." label="From address" bind:value={form.smtpFromAddress} />
            </div>
          </SettingsSection>
        </form>

        <form
          onsubmit={(e) => {
            e.preventDefault();
            run('instantSmtp');
          }}
        >
          <SettingsSection title="SMTP server">
            <div class="grid gap-4 lg:grid-cols-3">
              <div class="lg:col-span-3">
                <div class="w-full sm:w-72">
                  <Listbox id="smtpEnabled" label="SMTP delivery" bind:value={form.smtpEnabled} onchange={() => run('instantSmtp')} options={onOff} />
                </div>
              </div>
              <FormInput required id="smtpHost" placeholder="smtp.mailgun.org" label="Host" bind:value={form.smtpHost} />
              <FormInput required id="smtpPort" type="number" placeholder="587" label="Port" bind:value={form.smtpPort} />
              <Listbox
                required
                id="smtpEncryption"
                label="Encryption"
                bind:value={form.smtpEncryption}
                options={[
                  { value: 'starttls', label: 'StartTLS' },
                  { value: 'tls', label: 'TLS / SSL' },
                  { value: 'none', label: 'None' },
                ]}
              />
              <FormInput id="smtpUsername" label="Username" bind:value={form.smtpUsername} />
              <FormInput id="smtpPassword" type="password" label="Password" autocomplete="new-password" bind:value={form.smtpPassword} />
              <FormInput id="smtpTimeout" type="number" helper="Maximum delivery time in seconds." label="Timeout" bind:value={form.smtpTimeout} />
              <FormInput
                id="smtpEhloDomain"
                placeholder="coolify.example.com"
                helper="Fully qualified domain sent in the SMTP EHLO command. Uses the system default when empty."
                label="EHLO domain"
                bind:value={form.smtpEhloDomain}
              />
            </div>
          </SettingsSection>
        </form>

        <form
          onsubmit={(e) => {
            e.preventDefault();
            run('instantResend');
          }}
        >
          <SettingsSection title="Resend">
            <div class="grid gap-4 lg:grid-cols-2">
              <Listbox id="resendEnabled" label="Resend delivery" bind:value={form.resendEnabled} onchange={() => run('instantResend')} options={onOff} />
              <FormInput type="password" id="resendApiKey" placeholder="API key" required={!!form.resendEnabled} label="API key" autocomplete="new-password" bind:value={form.resendApiKey} />
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
      <div class="flex justify-end border-t border-neutral-200 pt-4 dark:border-white/[0.08]">
        <button type="submit" class="button">Send email</button>
      </div>
    </form>
  </Modal>
{/if}
