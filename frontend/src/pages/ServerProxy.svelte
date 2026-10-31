<script lang="ts">
  // Port of upstream livewire/server/proxy/show.blade.php and livewire/server/proxy.blade.php.
  import Reicon from '../components/Reicon.svelte';
  import Callout from '../components/Callout.svelte';
  import ConfirmModal from '../components/ConfirmModal.svelte';
  import Empty from '../components/Empty.svelte';
  import FormInput from '../components/FormInput.svelte';
  import FormTextarea from '../components/FormTextarea.svelte';
  import Listbox from '../components/Listbox.svelte';
  import MonacoEditor from '../components/MonacoEditor.svelte';
  import ServerFrame from '../components/ServerFrame.svelte';
  import SettingsSection from '../components/SettingsSection.svelte';
  import StatusBadge from '../components/StatusBadge.svelte';
  import UnsavedBar from '../components/UnsavedBar.svelte';
  import { ApiError } from '../lib/api';
  import type { ServerFacts } from '../lib/api';
  import { get, post, failure } from '../lib/serverApi';
  import { session } from '../lib/session.svelte';
  import { notify } from '../lib/toast.svelte';

  let { uuid }: { uuid: string } = $props();
  type Data = {
    proxy_type: string | null;
    status: string | null;
    redirect_enabled: boolean;
    redirect_url: string | null;
    custom_error_page: string | null;
    generate_exact_labels: boolean;
    certificate_resolver: string | null;
    pending: boolean;
    changed: boolean;
    is_swarm: boolean;
    configuration_path: string;
    error_page_kb: number;
    traefik: { version: string | null; detected: boolean; latest: string | null; outdated: boolean; newer_branch: string | null; newer_version: string | null };
  };
  type Form = { redirectUrl: string; certificateResolver: string; customErrorPage: string; proxySettings: string };

  let data = $state<Data | null>(null);
  let facts = $state<ServerFacts | null>(null);
  let form = $state<Form>({ redirectUrl: '', certificateResolver: '', customErrorPage: '', proxySettings: '' });
  let saved = $state<Form>({ redirectUrl: '', certificateResolver: '', customErrorPage: '', proxySettings: '' });
  let exactLabels = $state(false);
  let redirectEnabled = $state(true);
  let loadingConfig = $state(false);
  let saving = $state(false);
  let error = $state<ApiError | null>(null);
  let confirm = $state<'switch' | 'reset' | null>(null);
  const base = $derived(`/server-page/${encodeURIComponent(uuid)}/proxy`);
  const canEdit = $derived(session.isAdmin);
  const dirty = $derived(JSON.stringify(form) !== JSON.stringify(saved));
  const headline = (s: string) => s.replace(/[_-]+/g, ' ').replace(/\b\w/g, (c) => c.toUpperCase());
  const title = (s: string) => s.charAt(0).toUpperCase() + s.slice(1).toLowerCase();
  const fileProxy = $derived(data?.proxy_type === 'TRAEFIK' || data?.proxy_type === 'CADDY');

  async function load(f?: ServerFacts) {
    if (f) facts = f;
    if (facts && !facts.functional) return;
    const d = await get<Data>(base);
    data = d;
    exactLabels = d.generate_exact_labels;
    redirectEnabled = d.redirect_enabled;
    const next = { redirectUrl: d.redirect_url ?? '', certificateResolver: d.certificate_resolver ?? '', customErrorPage: d.custom_error_page ?? '', proxySettings: form.proxySettings };
    form = { ...next };
    saved = { ...next };
    if (d.proxy_type === 'TRAEFIK' || d.proxy_type === 'CADDY') loadConfiguration();
  }
  async function loadConfiguration() {
    loadingConfig = true;
    try {
      const r = await get<{ configuration: string | null }>(`${base}/configuration`);
      form.proxySettings = r.configuration ?? '';
      saved.proxySettings = form.proxySettings;
    } catch (e) {
      failure(e);
    } finally {
      loadingConfig = false;
    }
  }

  const routing = () => ({
    generate_exact_labels: exactLabels,
    redirect_url: form.redirectUrl || null,
    certificate_resolver: form.certificateResolver || null,
    custom_error_page: form.customErrorPage || null,
  });

  async function instant() {
    error = null;
    try {
      await post(`${base}/instant`, routing());
      notify.success('Settings saved.');
    } catch (e) {
      error = failure(e);
    }
  }
  async function saveRedirect() {
    try {
      await post(`${base}/redirect`, { redirect_enabled: redirectEnabled });
      notify.success('Proxy configuration saved.');
    } catch (e) {
      failure(e);
    }
  }
  async function resetErrorPage() {
    try {
      await post(`${base}/reset-error-page`);
      form.customErrorPage = saved.customErrorPage = '';
      notify.success('Default error page restored.');
    } catch (e) {
      failure(e);
    }
  }
  async function submit() {
    saving = true;
    error = null;
    try {
      await post(base, { ...routing(), ...(form.proxySettings ? { proxy_settings: form.proxySettings } : {}) });
      saved = { ...form };
      notify.success('Proxy configuration saved.');
      await load();
    } catch (e) {
      error = failure(e);
    } finally {
      saving = false;
    }
  }
  async function resetConfiguration() {
    try {
      const r = await post<{ message: string; configuration: string }>(`${base}/reset-configuration`);
      form.proxySettings = saved.proxySettings = r.configuration;
      notify.success(r.message);
    } catch (e) {
      failure(e);
    }
  }
  async function select(type: string) {
    try {
      await post(`${base}/select`, { proxy_type: type });
      window.location.reload();
    } catch (e) {
      failure(e);
    }
  }
  async function switchProxy() {
    try {
      await post(`${base}/change`);
      window.location.reload();
    } catch (e) {
      failure(e);
    }
  }
  const options = [
    { value: 'NONE', title: 'Custom', description: 'Manage the proxy outside Coolify.' },
    { value: 'TRAEFIK', title: 'Traefik', description: 'Use the default Coolify proxy.' },
    { value: 'CADDY', title: 'Caddy', description: 'Use the Coolify Caddy integration.' },
  ];
</script>

{#snippet chooser()}
  <SettingsSection title="Proxy configuration" description="Choose the reverse proxy implementation for this server.">
    {#if canEdit}
      <div class="grid gap-3 lg:grid-cols-3">
        {#each options as o (o.value)}
          <button type="button" onclick={() => select(o.value)} class="rounded-lg p-4 text-left ring-1 ring-neutral-200 transition-colors hover:bg-neutral-50 dark:ring-white/[0.08] dark:hover:bg-white/[0.04]">
            <p class="text-sm font-medium text-neutral-950 dark:text-fg">{o.title}</p>
            <p class="mt-1 text-xs leading-5 text-neutral-500 dark:text-fg-dim">{o.description}</p>
          </button>
        {/each}
      </div>
    {/if}
  </SettingsSection>
{/snippet}

<ServerFrame {uuid} active="proxy" sub="configuration" title="Proxy Configuration" onloaded={load}>
  {#snippet children(f)}
    {#if !f.functional}
      <div class="application-settings-form w-full">
        <SettingsSection title="Proxy" description="Configure the reverse proxy for this server.">
          <Empty size="sm" title="Server validation required" description="Validate this server before configuring its proxy." iconName="servers" />
        </SettingsSection>
      </div>
    {:else if data}
      <div class="w-full">
        <div class="application-settings-form flex w-full flex-col gap-6">
          {#if !data.proxy_type}
            {@render chooser()}
          {:else if data.proxy_type === 'NONE'}
            <SettingsSection title="Custom proxy" description="Coolify will not manage a reverse proxy for this server.">
              {#snippet actions()}
                {#if canEdit}
                  <button type="button" class="button" onclick={switchProxy}>Switch proxy</button>
                {/if}
              {/snippet}
              <Callout type="info" title="Custom proxy selected">Configure and operate the proxy outside Coolify.</Callout>
            </SettingsSection>
          {:else}
            <form
              class="contents"
              onsubmit={(e) => {
                e.preventDefault();
                submit();
              }}
            >
              <UnsavedBar {dirty} {saving} onsave={submit} onreset={() => ((form = { ...saved }), (error = null))} />
              <SettingsSection id="server-proxy-overview-section" title="Proxy configuration" description="Configure the reverse proxy and request handling for this server.">
                {#snippet actions()}
                  <div class="flex items-center gap-2">
                    <StatusBadge label={headline(data?.status ?? '')} type={(data?.status ?? '').includes('running') ? 'success' : 'neutral'} />
                    {#if data?.status === 'exited' || data?.status === 'removing'}
                      {#if canEdit}
                        <button type="button" class="button" onclick={() => (confirm = 'switch')}>Switch proxy</button>
                      {/if}
                    {:else}
                      <button type="button" class="button" disabled={!canEdit} onclick={() => notify.failure('The running proxy must be stopped before switching.')}>Switch proxy</button>
                    {/if}
                  </div>
                {/snippet}
                {#if data.changed}
                  <Callout type="warning" title="Your configuration changed, please restart the proxy."></Callout>
                {:else if data.pending}
                  <Callout type="warning" title="Restart required">Restart the proxy to apply TLS certificate changes.</Callout>
                {:else}
                  <div class="flex items-start gap-3">
                    <div class="flex size-9 shrink-0 items-center justify-center rounded-lg bg-neutral-100 text-neutral-500 dark:bg-white/[0.06] dark:text-fg-dim">
                      <Reicon name="servers" class="size-4" />
                    </div>
                    <div>
                      <p class="text-sm font-medium text-neutral-950 dark:text-fg">{title(data.proxy_type)}</p>
                      <p class="mt-1 text-xs text-neutral-500 dark:text-fg-dim">Saved and running configuration are synchronized.</p>
                    </div>
                  </div>
                {/if}
              </SettingsSection>

              <SettingsSection id="server-proxy-routing-section" title="Routing behavior" description="Control generated labels and requests that do not match a running resource.">
                <div class="grid gap-4 lg:grid-cols-2">
                  <Listbox
                    id="generateExactLabels"
                    label="Generated labels"
                    helper="<ul class='list-disc space-y-1 pl-4'><li><span class='font-semibold'>All supported proxies:</span> Traefik and Caddy labels are both generated, so switching the proxy keeps routing working.</li><li><span class='font-semibold'>Active proxy only:</span> generates fewer labels, but all resources must be redeployed to become accessible again after each proxy switch.</li></ul>"
                    bind:value={exactLabels}
                    onchange={instant}
                    options={[
                      { value: false, label: 'Labels for all supported proxies' },
                      { value: true, label: 'Labels for the active proxy only' },
                    ]}
                  />
                  <Listbox
                    id="redirectEnabled"
                    label="Unknown requests"
                    helper="Control the 503 response for unknown hosts and stopped services. Custom request handling shows an error page or redirects to a URL."
                    bind:value={redirectEnabled}
                    onchange={saveRedirect}
                    options={[
                      { value: false, label: 'Return the default 503 response' },
                      { value: true, label: 'Use custom request handling' },
                    ]}
                  />
                  {#if redirectEnabled}
                    <FormInput disabled={!canEdit} placeholder="https://app.coolify.io" id="redirectUrl" label="Redirect URL" helper="Leave empty to return a 503 response instead of redirecting." bind:value={form.redirectUrl} error={error?.field('redirect_url')} />
                  {/if}
                  {#if data.proxy_type === 'TRAEFIK'}
                    <FormInput
                      disabled={!canEdit}
                      placeholder="letsencrypt"
                      id="certificateResolver"
                      label="Certificate resolver"
                      helper="Traefik certificate resolver for generated HTTPS routes. Leave empty if Traefik uses your own certificates (for example Cloudflare Origin CA certificates in a dynamic configuration). Redeploy resources to apply the change."
                      bind:value={form.certificateResolver}
                      error={error?.field('certificate_resolver')}
                    />
                  {/if}
                </div>
                {#if redirectEnabled && !data.is_swarm}
                  <div class="mt-4 flex flex-col gap-2">
                    <FormTextarea
                      disabled={!canEdit}
                      id="customErrorPage"
                      rows={10}
                      monospace
                      label="Custom error page (HTML)"
                      placeholder="Leave empty to use the Coolify default error page."
                      helper={`One self-contained HTML file (max ${data.error_page_kb} KB) with inline CSS and images. Visitors see it with a 503 status for unknown domains and for stopped, deploying or unhealthy resources. A Redirect URL takes priority over this page.`}
                      bind:value={form.customErrorPage}
                      error={error?.field('custom_error_page')}
                    />
                    {#if canEdit}
                      <div class="flex gap-2">
                        <button type="button" class="button" onclick={resetErrorPage}>Reset to default</button>
                      </div>
                    {/if}
                  </div>
                {/if}
              </SettingsSection>

              {#if fileProxy}
                <SettingsSection id="server-proxy-file-section" title={data.proxy_type === 'TRAEFIK' ? 'Traefik configuration' : 'Caddy configuration'} description="Edit the generated proxy compose configuration used on this server.">
                  {#snippet actions()}
                    {#if canEdit && form.proxySettings}
                      <button type="button" class="button" onclick={() => (confirm = 'reset')}>Reset configuration</button>
                    {/if}
                  {/snippet}
                  {#if data.proxy_type === 'TRAEFIK'}
                    {#if data.traefik.version === 'latest'}
                      <Callout type="warning" title="Unpinned Traefik version">
                        The proxy uses the <span class="font-mono">latest</span> tag. Pin <span class="font-mono">traefik:{data.traefik.latest}</span> for predictable updates.
                      </Callout>
                    {/if}
                    {#if data.traefik.outdated}
                      <Callout type="warning" title="Traefik patch update available">
                        {data.traefik.detected ? 'Running version' : 'Configured image'}
                        v{(data.traefik.version ?? '').replace(/^v/, '')}. The latest patch for this branch is {data.traefik.latest}. Test the update before applying it to production servers.
                        <a class="font-medium underline" href="https://coolify.io/docs/core/networking/proxy/traefik/update" target="_blank">Learn how to update Traefik.</a>
                      </Callout>
                    {/if}
                    {#if data.traefik.newer_branch}
                      <Callout type="info" title="New Traefik minor version available">
                        {data.traefik.newer_branch} is available (latest patch: {data.traefik.newer_version}). Review the Traefik changelog for breaking changes before upgrading.
                        <a class="font-medium underline" href="https://coolify.io/docs/core/networking/proxy/traefik/update" target="_blank">Learn how to update Traefik.</a>
                      </Callout>
                    {/if}
                  {/if}
                  {#if loadingConfig}
                    <div role="status" class="mt-4 flex flex-col gap-2">
                      <span class="sr-only">Loading proxy configuration</span>
                    </div>
                  {:else if form.proxySettings}
                    <div class="relative mt-4" aria-live="polite">
                      <!-- svelte-ignore a11y_label_has_associated_control -->
                      <label class="flex gap-1 items-center mb-1 text-sm font-medium text-black dark:text-white">Configuration file · {data.configuration_path}</label>
                      <MonacoEditor bind:value={form.proxySettings} language="yaml" rows={30} readonly={!canEdit} />
                    </div>
                  {/if}
                </SettingsSection>
              {/if}
            </form>
          {/if}
        </div>
      </div>
    {/if}
    {#if confirm === 'reset'}
      <ConfirmModal
        title="Reset Proxy Configuration?"
        actions={['Reset the proxy configuration to Coolify defaults.', 'Remove custom ports, entrypoints, and other manual changes.']}
        confirmationText={f.name}
        confirmationLabel="Confirm by entering the server name"
        shortConfirmationLabel="Server Name"
        step2ButtonText="Reset Configuration"
        onconfirm={resetConfiguration}
        onclose={() => (confirm = null)}
      />
    {:else if confirm === 'switch'}
      <ConfirmModal
        title="Confirm Proxy Switching?"
        actions={['Custom proxy configurations may be reset to their default settings.']}
        warningMessage="Review the proxy switching guide before continuing."
        step2ButtonText="Switch Proxy"
        confirmWithText={false}
        onconfirm={switchProxy}
        onclose={() => (confirm = null)}
      />
    {/if}
  {/snippet}
</ServerFrame>
