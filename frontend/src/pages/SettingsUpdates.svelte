<script lang="ts">
  // Port of upstream livewire/settings/updates.blade.php (App\Livewire\Settings\Updates).
  import FormInput from '../components/FormInput.svelte';
  import Listbox from '../components/Listbox.svelte';
  import Reicon from '../components/Reicon.svelte';
  import SettingsLayout from '../components/SettingsLayout.svelte';
  import SettingsSection from '../components/SettingsSection.svelte';
  import UnsavedBar from '../components/UnsavedBar.svelte';
  import UpgradeBadge from '../components/UpgradeBadge.svelte';
  import { ApiError } from '../lib/api';
  import { router } from '../lib/router.svelte';
  import { settingsUi, type ActionResult, type UpgradeState, type Values } from '../lib/settingsApi';
  import { notify } from '../lib/toast.svelte';

  document.title = 'Update Settings | coolify-rs';

  let form = $state<Values | null>(null);
  let saved = $state<Values>({});
  let autoupdate = $state<unknown>(null);
  let upgrade = $state<UpgradeState | null>(null);
  let saving = $state(false);
  let errors = $state<Record<string, string[]>>({});

  const targets = ['update_check_frequency', 'auto_update_frequency'];
  const dirty = $derived(!!form && targets.some((k) => (form![k] ?? '') !== (saved[k] ?? '')));
  const onOff = [
    { value: true, label: 'Enabled' },
    { value: false, label: 'Disabled' },
  ];

  settingsUi.updates().then((d) => {
    if (d.redirect) return router.go(d.redirect, true);
    autoupdate = d.autoupdate;
    upgrade = d.upgrade;
    apply(d.settings!);
  });

  function apply(s: Values) {
    saved = { ...s };
    form = { ...s };
  }

  function outcome(r: ActionResult) {
    if (r.errors) errors = r.errors;
    if (r.auto_update_frequency && form) form.auto_update_frequency = r.auto_update_frequency;
    if (r.settings) apply(r.settings);
    if (r.upgrade) upgrade = r.upgrade;
    if (r.error) notify.failure(r.error);
    if (r.message) notify.success(r.message);
  }

  async function run(action: 'submit' | 'instant' | 'check') {
    if (!form) return;
    saving = true;
    errors = {};
    try {
      outcome(await settingsUi.updatesAction(action, $state.snapshot(form)));
    } catch (e) {
      notify.failure(e instanceof ApiError ? e.message : String(e));
    } finally {
      saving = false;
    }
  }
</script>

<div>
  <SettingsLayout>
    {#if form}
      <form
        class="application-settings-form flex min-w-0 flex-col gap-6"
        onsubmit={(e) => {
          e.preventDefault();
          run('submit');
        }}
      >
        <UnsavedBar {dirty} {saving} onsave={() => run('submit')} onreset={() => form && (form = { ...saved })} />
        <SettingsSection title="Updates" description="Install the latest Coolify version manually when an update is available.">
          {#snippet actions()}
            <UpgradeBadge fullButton state={upgrade} />
            <button type="button" class="button" onclick={() => run('check')}>
              <Reicon name="refresh" class="size-3.5" />
              Check now
            </button>
          {/snippet}
          <div class="grid gap-4 lg:grid-cols-2">
            <FormInput
              required
              id="update_check_frequency"
              label="Check frequency"
              placeholder="0 * * * *"
              error={errors.update_check_frequency?.[0]}
              helper="A cron expression or preset such as hourly, daily, weekly, monthly, or yearly."
              bind:value={form.update_check_frequency}
            />
            <Listbox
              id="docker_registry_url"
              label="Docker registry"
              helper="Switch registries if the current source is rate limited."
              bind:value={form.docker_registry_url}
              options={[
                { value: 'docker.io', label: 'Docker Hub' },
                { value: 'ghcr.io', label: 'GitHub Container Registry' },
              ]}
            />
            {#if autoupdate !== null}
              <Listbox disabled id="is_auto_update_enabled" label="Automatic updates" helper="Controlled by the AUTOUPDATE environment variable." bind:value={form.is_auto_update_enabled} options={onOff} />
            {:else}
              <Listbox id="is_auto_update_enabled" label="Automatic updates" bind:value={form.is_auto_update_enabled} onchange={() => run('instant')} options={onOff} />
            {/if}
            {#if autoupdate === null && form.is_auto_update_enabled}
              <FormInput
                required
                id="auto_update_frequency"
                label="Update frequency"
                placeholder="0 0 * * *"
                error={errors.auto_update_frequency?.[0]}
                helper="Cron expression or preset for installing updates."
                bind:value={form.auto_update_frequency}
              />
            {:else}
              <FormInput id="update-frequency-disabled" label="Update frequency" disabled placeholder="Disabled" value="" />
            {/if}
          </div>
        </SettingsSection>
      </form>
    {/if}
  </SettingsLayout>
</div>
