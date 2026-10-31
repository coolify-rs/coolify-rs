<script lang="ts">
  // Port of upstream livewire/settings/index.blade.php (App\Livewire\Settings\Index) and
  // components/domain-conflict-modal.blade.php.
  import Callout from '../components/Callout.svelte';
  import FormInput from '../components/FormInput.svelte';
  import Listbox from '../components/Listbox.svelte';
  import Reicon from '../components/Reicon.svelte';
  import SettingsLayout from '../components/SettingsLayout.svelte';
  import SettingsSection from '../components/SettingsSection.svelte';
  import UnsavedBar from '../components/UnsavedBar.svelte';
  import { ApiError } from '../lib/api';
  import { router } from '../lib/router.svelte';
  import { settingsUi, type ActionResult, type DomainConflict, type Values } from '../lib/settingsApi';
  import { notify } from '../lib/toast.svelte';

  document.title = 'Settings | coolify-rs';

  let form = $state<Values | null>(null);
  let saved = $state<Values>({});
  let timezones = $state<string[]>([]);
  let saving = $state(false);
  let conflicts = $state<DomainConflict[]>([]);
  let errors = $state<Record<string, string[]>>({});

  // `targets="fqdn,instance_name,public_ipv4,public_ipv6,dev_helper_version"`.
  const targets = ['fqdn', 'instance_name', 'public_ipv4', 'public_ipv6', 'dev_helper_version'];
  const dirty = $derived(!!form && targets.some((k) => (form![k] ?? '') !== (saved[k] ?? '')));
  const https = $derived((form?.fqdn ?? '').toLowerCase().startsWith('https://'));

  settingsUi.general().then((d) => {
    if (d.redirect) return router.go(d.redirect, true);
    timezones = d.timezones;
    apply(d.settings!);
  });

  function apply(s: Values) {
    saved = { ...s };
    form = { ...s };
  }

  function outcome(r: ActionResult) {
    if (r.errors) errors = r.errors;
    if (r.instance_timezone && form) form.instance_timezone = r.instance_timezone;
    if (r.settings) apply(r.settings);
    if (r.dns_error) notify.failure(r.dns_error);
    if (r.conflicts) conflicts = r.conflicts;
    if (r.error) notify.failure(r.error);
    if (r.message) notify.success(r.message);
  }

  async function run(action: 'submit' | 'confirm' | 'detect') {
    if (!form || saving) return;
    saving = true;
    errors = {};
    try {
      outcome(await settingsUi.generalAction(action, $state.snapshot(form)));
    } catch (e) {
      notify.failure(e instanceof ApiError ? e.message : String(e));
    } finally {
      saving = false;
    }
  }
</script>

<svelte:window onkeydown={(e) => e.key === 'Escape' && (conflicts = [])} />

<div>
  <SettingsLayout>
    {#if form}
      <form
        class="application-settings-form flex w-full min-w-0 flex-col gap-6"
        onsubmit={(e) => {
          e.preventDefault();
          run('submit');
        }}
      >
        <UnsavedBar {dirty} {saving} onsave={() => run('submit')} onreset={() => form && (form = { ...saved })} />
        <SettingsSection title="General">
          <div class="grid gap-4 lg:grid-cols-2">
            <div class={https ? '' : 'lg:col-span-2'}>
              <FormInput
                id="fqdn"
                label="URL"
                error={errors.fqdn?.[0]}
                helper="Enter the full URL of the instance (for example, https://dashboard.example.com).<br><br><span class='text-coollabs dark:text-brand-light'>Important:</span> Include <b>https://</b> to secure the dashboard with HTTPS.<br><br>Passkeys are tied to this domain. If you change it, users must add their passkeys again."
                placeholder="https://coolify.yourdomain.com"
                bind:value={form.fqdn}
              />
            </div>
            {#if https}
              <div>
                <Listbox
                  id="is_dashboard_force_https_enabled"
                  label="Redirect HTTP to HTTPS"
                  helper="Disable only when Cloudflare Tunnel or another proxy connects to Coolify over HTTP. Keep enabled when Cloudflare uses Full or Full (Strict) SSL."
                  bind:value={form.is_dashboard_force_https_enabled}
                  onchange={() => run('submit')}
                  options={[
                    { value: true, label: 'Enabled' },
                    { value: false, label: 'Disabled' },
                  ]}
                />
              </div>
            {/if}
            <FormInput id="instance_name" label="Name" placeholder="Coolify" helper="Custom name for this Coolify instance." bind:value={form.instance_name} />
            <Listbox
              searchable
              id="instance_timezone"
              label="Instance timezone"
              helper="Timezone used for update checks and the automatic update schedule."
              searchPlaceholder="Search timezones"
              searchEmptyText="No matching timezone"
              bind:value={form.instance_timezone}
              onchange={() => run('submit')}
              options={timezones.map((t) => ({ value: t, label: t }))}
            />
          </div>
        </SettingsSection>

        <SettingsSection title="Network addresses">
          {#snippet actions()}
            <button type="button" class="button" onclick={() => run('detect')}>Detect</button>
          {/snippet}
          <div class="grid gap-4 lg:grid-cols-2">
            <FormInput
              id="public_ipv4"
              type="password"
              label="Instance public IPv4"
              helper="Coolify detects this address on start only when the field is empty. Set it when the detected address is wrong. Detect looks it up again and saves it."
              placeholder="1.2.3.4"
              autocomplete="new-password"
              bind:value={form.public_ipv4}
            />
            <FormInput
              id="public_ipv6"
              type="password"
              label="Instance public IPv6"
              helper="Coolify detects this address on start only when the field is empty. Set it when the detected address is wrong. Detect looks it up again and saves it."
              placeholder="2001:db8::1"
              autocomplete="new-password"
              bind:value={form.public_ipv6}
            />
          </div>
        </SettingsSection>
      </form>
    {/if}

    {#if conflicts.length}
      <div class="relative w-auto h-auto z-40">
        <div class="fixed inset-0 z-99 flex min-h-full items-center justify-center overflow-y-auto p-4">
          <div class="absolute inset-0 bg-black/50 backdrop-blur-[2px]"></div>
          <div
            class="application-settings-form application-settings-section relative w-full lg:min-w-[36rem] lg:max-w-2xl"
            style="box-shadow: 0 0 0 1px var(--coollabs-hairline), var(--shadow-modal)"
          >
            <header>
              <h3>Domain already in use</h3>
              <button
                type="button"
                aria-label="Close"
                onclick={() => (conflicts = [])}
                class="flex size-7 items-center justify-center rounded-md text-neutral-500 transition-colors hover:bg-neutral-100 hover:text-black dark:text-fg-faint dark:hover:bg-white/[0.06] dark:hover:text-fg"
              >
                <Reicon name="x" class="size-4" />
              </button>
            </header>
            <div class="application-settings-section-body">
              <Callout type="danger" title="Domain conflict detected" class="mb-4">
                The following domain(s) are already in use by other resources. Using the same domain for multiple resources can cause routing conflicts and unpredictable behavior.
              </Callout>
              <div class="mb-4">
                <ul class="space-y-2">
                  {#each conflicts as c}
                    <li class="flex items-start text-[12px] leading-5 text-red-600 dark:text-red-400">
                      <div>
                        <strong>{c.domain}</strong> is used by
                        {#if c.resource_type === 'instance'}
                          <strong>{c.resource_name}</strong>
                        {:else}
                          <a href={c.resource_link} target="_blank" class="underline hover:text-red-400">{c.resource_name}</a>
                        {/if}
                        ({c.resource_type}{#if c.service_name}: {c.service_name}{/if})
                      </div>
                    </li>
                  {/each}
                </ul>
              </div>
              <Callout type="warning" title="What will happen if you continue?" class="mb-4">
                <ul class="mt-2 ml-4 list-disc">
                  <li>The Coolify instance domain will conflict with existing resources.</li>
                  <li>SSL certificates might not work correctly.</li>
                  <li>Routing behavior will be unpredictable.</li>
                  <li>You may not be able to access the Coolify dashboard properly.</li>
                </ul>
              </Callout>
              <div class="mt-4 flex flex-wrap justify-end gap-2 border-t border-neutral-200 pt-4 dark:border-white/[0.08]">
                <button type="button" class="button" onclick={() => (conflicts = [])}>Cancel</button>
                <button
                  type="button"
                  class="button button-error"
                  onclick={() => {
                    conflicts = [];
                    run('confirm');
                  }}>Proceed anyway</button
                >
              </div>
            </div>
          </div>
        </div>
      </div>
    {/if}
  </SettingsLayout>
</div>
