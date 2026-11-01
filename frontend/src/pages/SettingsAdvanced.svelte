<script lang="ts">
  // Port of upstream livewire/settings/advanced.blade.php (App\Livewire\Settings\Advanced).
  import Callout from '../components/Callout.svelte';
  import FormInput from '../components/FormInput.svelte';
  import FormTextarea from '../components/FormTextarea.svelte';
  import Listbox from '../components/Listbox.svelte';
  import Reicon from '../components/Reicon.svelte';
  import SettingsLayout from '../components/SettingsLayout.svelte';
  import SettingsSection from '../components/SettingsSection.svelte';
  import SettingsSubsection from '../components/SettingsSubsection.svelte';
  import UnsavedBar from '../components/UnsavedBar.svelte';
  import { ApiError } from '../lib/api';
  import { link, router } from '../lib/router.svelte';
  import { settingsUi, type ActionResult, type Values } from '../lib/settingsApi';
  import { notify } from '../lib/toast.svelte';

  document.title = 'Advanced Settings | coolify-rs';

  let form = $state<Values | null>(null);
  let saved = $state<Values>({});
  let options = $state<{ value: string; label: string }[]>([]);
  let mcpUrl = $state('');
  let saving = $state(false);
  let copied = $state(false);

  const targets = ['custom_dns_servers', 'allowed_ips', 'webhook_allowed_internal_hosts', 'webhook_allow_localhost', 'image_cdn_url'];
  const dirty = $derived(!!form && targets.some((k) => (form![k] ?? '') !== (saved[k] ?? '')));
  const onOff = [
    { value: true, label: 'Enabled' },
    { value: false, label: 'Disabled' },
  ];
  const openApi = $derived(!!form?.is_api_enabled && (!form.allowed_ips || String(form.allowed_ips).split(',').map((s) => s.trim()).includes('0.0.0.0')));

  settingsUi.advanced().then((d) => {
    if (d.redirect) return router.go(d.redirect, true);
    options = d.storage_options;
    mcpUrl = d.mcp_url;
    apply(d.settings!);
  });

  function apply(s: Values) {
    saved = { ...s };
    form = { ...s };
  }

  function outcome(r: ActionResult) {
    if (r.settings) apply(r.settings);
    if (r.file_download_storage && form) form.file_download_storage = r.file_download_storage;
    if (r.error) notify.failure(r.error);
    if (r.message) notify.success(r.message);
  }

  async function run(action: 'submit' | 'instant') {
    if (!form) return;
    saving = true;
    try {
      outcome(await settingsUi.advancedAction(action, $state.snapshot(form)));
    } catch (e) {
      notify.failure(e instanceof ApiError ? e.message : String(e));
    } finally {
      saving = false;
    }
  }
  const instant = () => run('instant');

  async function copyMcp() {
    await navigator.clipboard?.writeText(mcpUrl).catch(() => {});
    copied = true;
    setTimeout(() => (copied = false), 1000);
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

        <SettingsSection id="access-section" title="Access and API">
          <div class="grid gap-4 lg:grid-cols-2">
            <Listbox
              id="is_registration_enabled"
              label="Registration"
              helper="Allow users to create their own account. When disabled, only administrators can create accounts."
              bind:value={form.is_registration_enabled}
              onchange={instant}
              options={[
                { value: true, label: 'Anyone can register' },
                { value: false, label: 'Registration disabled' },
              ]}
            />
            <Listbox
              id="disable_registration_when_oauth_enabled"
              label="Password registration with OAuth"
              helper="Hide password registration whenever at least one OAuth provider is enabled."
              bind:value={form.disable_registration_when_oauth_enabled}
              onchange={instant}
              options={[
                { value: false, label: 'Allow password registration' },
                { value: true, label: 'Disable when OAuth is enabled' },
              ]}
            />
            <Listbox
              id="disable_two_step_confirmation"
              label="Destructive action confirmation"
              helper="Choose whether destructive actions require password and text confirmation."
              bind:value={form.disable_two_step_confirmation}
              onchange={instant}
              options={[
                { value: false, label: 'Require two-step confirmation' },
                { value: true, label: 'Skip two-step confirmation' },
              ]}
            />
            <Listbox
              id="is_passkey_enabled"
              label="Passkeys"
              helper="Let users sign in with a passkey. Passkeys need an HTTPS instance URL. Stored passkeys are kept when this is disabled."
              bind:value={form.is_passkey_enabled}
              onchange={instant}
              options={onOff}
            />
          </div>
          <SettingsSubsection id="api-section" title="API and MCP">
            <div class="grid gap-4 lg:grid-cols-2">
              <Listbox id="is_api_enabled" label="API access" helper="Allow authenticated requests to the coolify-rs REST API." bind:value={form.is_api_enabled} onchange={instant} options={onOff} />
              <Listbox
                id="is_mcp_server_enabled"
                label="MCP server"
                helper="Expose the authenticated Streamable HTTP endpoint at /mcp."
                bind:value={form.is_mcp_server_enabled}
                onchange={instant}
                options={onOff}
              />
              <div class="lg:col-span-2">
                <FormInput
                  id="allowed_ips"
                  label="Allowed IPs"
                  helper="Applies to the REST API and the MCP endpoint. Comma-separated IPs or CIDR ranges. Empty or 0.0.0.0 allows all sources."
                  placeholder="192.168.1.100, 10.0.0.0/8"
                  bind:value={form.allowed_ips}
                />
              </div>
            </div>
            {#if openApi}
              <Callout type="warning" title="API access is open to every source" class="mt-4">Restrict the allowlist before using API access on a public production instance.</Callout>
            {/if}
            {#if form.is_mcp_server_enabled}
              <Callout type="info" title="MCP endpoint" class="mt-4">
                <div class="flex items-center gap-1">
                  <code>{mcpUrl}</code>
                  <button type="button" title="Copy MCP endpoint" aria-label="Copy MCP endpoint" class="icon-button group shrink-0" onclick={copyMcp}>
                    <span class="inline-flex transition-transform duration-150 ease-out group-active:scale-75">
                      <Reicon name={copied ? 'check' : 'copy'} class="size-3.5 {copied ? 'text-success' : ''}" />
                    </span>
                  </button>
                </div>
                Authenticate with a bearer token from
                <a href="/security/api-tokens" use:link class="font-medium underline">Keys & Tokens → API Tokens</a>.
              </Callout>
            {/if}
          </SettingsSubsection>
        </SettingsSection>

        <SettingsSection id="network-section" title="Network" description="Domain validation and the internal targets that outbound requests may reach.">
          <SettingsSubsection first id="dns-section" title="DNS validation">
            <div class="grid gap-4 lg:grid-cols-2">
              <Listbox id="is_dns_validation_enabled" label="DNS validation" helper="Validate custom domains before deployment." bind:value={form.is_dns_validation_enabled} onchange={instant} options={onOff} />
              <FormInput id="custom_dns_servers" label="Custom DNS servers" helper="Comma-separated resolvers. Leave empty to use system defaults." placeholder="1.1.1.1, 8.8.8.8" bind:value={form.custom_dns_servers} />
            </div>
          </SettingsSubsection>
          <SettingsSubsection id="endpoint-section" title="Outbound endpoints">
            <div class="flex flex-col gap-4">
              <FormTextarea
                id="webhook_allowed_internal_hosts"
                rows={4}
                label="Allowed internal targets"
                helper="Hostnames, IPs, or CIDR ranges separated by commas or new lines."
                placeholder="hooks.company.local, 10.50.0.0/16"
                bind:value={form.webhook_allowed_internal_hosts}
              />
              <div class="max-w-md">
                <Listbox
                  id="webhook_allow_localhost"
                  label="Localhost targets"
                  helper="Loopback targets must also be present in the allowlist."
                  bind:value={form.webhook_allow_localhost}
                  options={[
                    { value: true, label: 'Allowed' },
                    { value: false, label: 'Blocked' },
                  ]}
                />
              </div>
            </div>
          </SettingsSubsection>
        </SettingsSection>

        <SettingsSection id="interface-section" title="Interface and telemetry">
          <div class="grid gap-4 lg:grid-cols-2">
            <Listbox
              id="is_wire_navigate_enabled"
              label="Navigation"
              helper="Prefetch pages and navigate without full reloads."
              bind:value={form.is_wire_navigate_enabled}
              onchange={instant}
              options={[
                { value: true, label: 'SPA navigation' },
                { value: false, label: 'Full page navigation' },
              ]}
            />
            <Listbox
              id="do_not_track"
              label="Anonymous telemetry"
              helper="Control installation counting and error reports."
              bind:value={form.do_not_track}
              onchange={instant}
              options={[
                { value: false, label: 'Enabled' },
                { value: true, label: 'Disabled' },
              ]}
            />
            <div class="flex flex-col gap-2">
              <Listbox
                id="is_sponsorship_popup_enabled"
                label="Sponsorship reminders"
                helper="Show the monthly project sponsorship reminder."
                bind:value={form.is_sponsorship_popup_enabled}
                onchange={instant}
                options={onOff}
              />
            </div>
          </div>
        </SettingsSection>

        <SettingsSection id="storage-section" title="Storage" description="Where coolify-rs stores uploaded images and how users download files.">
          <SettingsSubsection
            first
            id="avatar-storage-section"
            title="Image storage"
            helper="Choose where compressed profile pictures and project icons are stored. Use S3 for multi-instance or cloud deployments so every application replica can access the same files."
          >
            <div class="flex max-w-md flex-col gap-4">
              <Listbox id="avatar_storage" label="Storage destination" bind:value={form.avatar_storage} onchange={instant} {options} />
              <FormInput
                id="image_cdn_url"
                label="Image CDN URL"
                helper="Optional public URL (for example a CDN or public bucket domain) that serves profile pictures and project icons stored on S3. Leave empty to serve them through coolify-rs."
                placeholder="https://images.example.com"
                bind:value={form.image_cdn_url}
              />
            </div>
            {#if options.length === 1}
              <Callout type="info" title="No usable S3 storage configured" class="mt-4">Add and test an S3-compatible storage under Storages before selecting it here.</Callout>
            {/if}
          </SettingsSubsection>
          <SettingsSubsection
            id="file-download-storage-section"
            title="File downloads"
            helper="Choose how files from the container file browser and backups are downloaded. Streaming sends each file through this coolify-rs instance (file browser: up to 1 GB). With S3, the server uploads the file to the bucket and the browser downloads it from S3, up to 5 GB; larger backups are streamed. S3 download links expire after 15 minutes, and the files are deleted after 6 hours."
          >
            <div class="flex max-w-md flex-col gap-4">
              <Listbox
                id="file_download_storage"
                label="Download method"
                bind:value={form.file_download_storage}
                onchange={instant}
                options={[{ value: 'stream', label: 'Stream through coolify-rs' }, ...options.filter((o) => o.value !== 'local')]}
              />
            </div>
            <Callout type="info" title="Bucket auto-delete rule" class="mt-4">
              When you select an S3 storage, coolify-rs adds a lifecycle rule that deletes objects under
              <span class="font-mono">coolify-file-downloads/</span> after 1 day, as a safety net. The selected storage must be reachable from your servers, and the S3 provider must support lifecycle rules.
            </Callout>
          </SettingsSubsection>
        </SettingsSection>
      </form>
    {/if}
  </SettingsLayout>
</div>
