<script lang="ts">
  // Port of upstream livewire/source/github/change.blade.php with permissions.blade.php
  // and resources.blade.php (+ Source\Github\Change), including the manifest flow.
  import Callout from '../components/Callout.svelte';
  import ConfirmModal from '../components/ConfirmModal.svelte';
  import DangerZone from '../components/DangerZone.svelte';
  import FormInput from '../components/FormInput.svelte';
  import Listbox from '../components/Listbox.svelte';
  import Reicon from '../components/Reicon.svelte';
  import SettingsSection from '../components/SettingsSection.svelte';
  import SourceResources from '../components/SourceResources.svelte';
  import StatusBadge from '../components/StatusBadge.svelte';
  import UnsavedBar from '../components/UnsavedBar.svelte';
  import { ApiError } from '../lib/api';
  import { page } from '../lib/page.svelte';
  import { router } from '../lib/router.svelte';
  import { notify } from '../lib/toast.svelte';
  import { setSourceCrumbs, sourcesUi, type GithubSource } from '../lib/sourcesApi';

  let { uuid, tab = 'general' }: { uuid: string; tab?: string } = $props();

  type Form = {
    name: string;
    organization: string;
    isSystemWide: boolean;
    htmlUrl: string;
    apiUrl: string;
    customUser: string;
    customPort: string;
    appId: string;
    installationId: string;
    clientId: string;
    clientSecret: string;
    webhookSecret: string;
    privateKeyId: number;
  };
  const blank: Form = {
    name: '',
    organization: '',
    isSystemWide: false,
    htmlUrl: '',
    apiUrl: '',
    customUser: '',
    customPort: '',
    appId: '',
    installationId: '',
    clientId: '',
    clientSecret: '',
    webhookSecret: '',
    privateKeyId: 0,
  };
  let data = $state<GithubSource | null>(null);
  let form = $state<Form>({ ...blank });
  let saved = $state<Form>({ ...blank });
  let errors = $state<Record<string, string[]>>({});
  let saving = $state(false);
  let confirmDelete = $state(false);
  const dirty = $derived(JSON.stringify(form) !== JSON.stringify(saved));

  // The registration page's state.
  let useCustom = $state(false);
  let endpoint = $state('');
  let customEndpoint = $state('');
  let previewPermissions = $state(router.query('previews') !== '0');
  let githubRunners = $state(false);
  let endpointError = $state('');

  function apply(d: GithubSource) {
    data = d;
    const next: Form = {
      name: d.display_name,
      organization: d.organization ?? '',
      isSystemWide: d.is_system_wide,
      htmlUrl: d.html_url,
      apiUrl: d.api_url,
      customUser: d.custom_user,
      customPort: String(d.custom_port),
      appId: d.app_id == null ? '' : String(d.app_id),
      installationId: d.installation_id == null ? '' : String(d.installation_id),
      clientId: d.client_id ?? '',
      clientSecret: d.client_secret ?? '',
      webhookSecret: d.webhook_secret ?? '',
      privateKeyId: d.private_key_id ?? 0,
    };
    form = { ...next };
    saved = { ...next };
    const e = d.endpoints;
    endpoint = e.fqdn ?? e.ipv4 ?? e.ipv6 ?? e.app_url ?? '';
    githubRunners = !!d.organization && router.query('runners') === '1';
    document.title = `${d.name || 'GitHub App'} | Sources | coolify-rs`;
    setSourceCrumbs('github', d.uuid, d.has_installation);
    if (d.has_app_id) {
      const base = `/source/github/${d.uuid}`;
      page.navContext = {
        label: 'Sources',
        backTitle: 'Back to Integrations',
        ariaLabel: 'GitHub App settings',
        groups: [
          [
            { label: 'General', icon: 'settings', href: base, active: tab === 'general' },
            { label: 'Permissions', icon: 'keys', href: `${base}/permissions`, active: tab === 'permissions' },
            { label: 'Resources', icon: 'grid', href: `${base}/resources`, active: tab === 'resources' },
          ],
          [{ label: 'Danger Zone', icon: 'danger', href: `${base}/danger`, active: tab === 'danger' }],
        ],
      };
    }
  }

  async function load() {
    apply(await sourcesUi.github(uuid));
  }

  $effect(() => {
    load().catch((e) => notify.failure(e instanceof ApiError ? e.message : String(e)));
    const flash = router.query('error');
    if (flash) notify.failure(flash);
  });

  function body() {
    return {
      name: form.name,
      organization: form.organization,
      is_system_wide: form.isSystemWide,
      html_url: form.htmlUrl,
      api_url: form.apiUrl,
      custom_user: form.customUser,
      custom_port: form.customPort,
      app_id: form.appId,
      installation_id: form.installationId,
      client_id: form.clientId,
      client_secret: form.clientSecret,
      webhook_secret: form.webhookSecret,
      private_key_id: form.privateKeyId,
      contents: data?.contents,
      metadata: data?.metadata,
      pull_requests: data?.pull_requests,
    };
  }

  async function run(action: () => Promise<{ message?: string; info?: string }>) {
    try {
      const r = await action();
      if (r.message) notify.success(r.message);
      if (r.info) notify.info('Info', r.info);
      return true;
    } catch (e) {
      if (e instanceof ApiError && Object.keys(e.errors).length > 0) errors = e.errors;
      notify.failure(e instanceof ApiError ? e.message : String(e));
      return false;
    }
  }

  /** `submit()`. */
  async function submit() {
    if (saving) return;
    saving = true;
    errors = {};
    if (await run(() => sourcesUi.updateGithub(uuid, body()))) await load();
    saving = false;
  }

  async function testConnection() {
    await run(() => sourcesUi.test('github', uuid));
  }

  async function syncName() {
    const ok = await run(() => sourcesUi.githubSyncName(uuid, { app_id: form.appId, private_key_id: form.privateKeyId }));
    if (ok) await load();
  }

  async function checkPermissions() {
    if (await run(() => sourcesUi.githubPermissions(uuid))) await load();
  }

  async function manual() {
    if (await run(() => sourcesUi.githubManual(uuid))) await load();
  }

  async function remove(): Promise<string | void> {
    try {
      await sourcesUi.remove('github', uuid);
      router.go('/sources');
    } catch (e) {
      notify.failure(e instanceof ApiError ? e.message : String(e));
      confirmDelete = false;
    }
  }

  /** `createGithubApp()`: posts the App manifest to GitHub. */
  function register() {
    if (!data) return;
    const selected = endpoint.trim();
    const custom = customEndpoint.trim();
    if (useCustom && !custom) {
      notify.error('Error', 'Please enter a custom webhook endpoint.');
      return;
    }
    if (!useCustom && !selected) {
      notify.error('Error', 'Please enter a webhook endpoint.');
      return;
    }
    const baseUrl = (useCustom ? custom : selected).replace(/\/+$/, '');
    const webhookBaseUrl = `${baseUrl}/webhooks`;
    const organization = data.organization;
    const organizationPath = organization ? encodeURIComponent(organization.replace(/^\/+|\/+$/g, '')) : '';
    const path = organizationPath ? `organizations/${organizationPath}/settings/apps/new` : 'settings/apps/new';
    const default_permissions: Record<string, string> = { contents: 'read', metadata: 'read', emails: 'read', administration: 'read' };
    const default_events = ['push'];
    if (previewPermissions) {
      default_permissions.pull_requests = 'write';
      default_events.push('pull_request');
    }
    if (githubRunners && organization) {
      default_permissions.organization_self_hosted_runners = 'write';
      default_permissions.actions = 'read';
      default_events.push('workflow_job');
    }
    const manifest = {
      name: data.display_name,
      url: baseUrl,
      hook_attributes: { url: `${webhookBaseUrl}/source/github/events`, active: true },
      redirect_url: `${webhookBaseUrl}/source/github/redirect`,
      callback_urls: [`${baseUrl}/login/github/app`],
      public: false,
      request_oauth_on_install: false,
      setup_url: `${webhookBaseUrl}/source/github/install`,
      setup_on_update: true,
      default_permissions,
      default_events,
    };
    const f = document.createElement('form');
    f.setAttribute('method', 'post');
    f.setAttribute('action', `${data.html_url}/${path}?state=${data.manifest_state}`);
    const input = document.createElement('input');
    input.setAttribute('id', 'manifest');
    input.setAttribute('name', 'manifest');
    input.setAttribute('type', 'hidden');
    input.setAttribute('value', JSON.stringify(manifest));
    f.appendChild(input);
    document.getElementsByTagName('body')[0].appendChild(f);
    f.submit();
  }

  const err = (k: string) => errors[k]?.[0];
  const endpointOptions = $derived.by(() => {
    const e = data?.endpoints;
    return [e?.fqdn, e?.ipv4, e?.ipv6, e?.app_url].filter((v): v is string => !!v).map((v) => ({ value: v, label: `Use ${v}` }));
  });
  const keyOptions = $derived([
    ...(data?.private_key_id == null ? [{ value: '0', label: 'Select a private key' }] : []),
    ...(data?.private_keys ?? []).map((k) => ({ value: String(k.id), label: k.name })),
  ]);
  const inputsDisabled = $derived(!data?.can_update);
</script>

{#snippet externalLink()}
  <svg class="inline-flex w-3 h-3 dark:text-neutral-400 text-black" fill="none" stroke="currentColor" viewBox="0 0 24 24">
    <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M10 6H6a2 2 0 00-2 2v10a2 2 0 002 2h10a2 2 0 002-2v-4M14 4h6m0 0v6m0-6L10 14" />
  </svg>
{/snippet}

{#snippet deleteButton()}
  <button type="button" class="button button-error" onclick={() => (confirmDelete = true)}>Delete</button>
{/snippet}

{#if data}
  {#if data.has_app_id}
    <div class="mb-3 lg:hidden">
      <h1 class="min-w-0 truncate text-[24px]! leading-7! font-semibold! tracking-tight!">{form.name || 'GitHub App'}</h1>
      <p class="mt-1 text-[13px] text-neutral-500 dark:text-fg-dim">
        {data.organization ? `GitHub App for ${data.organization}` : 'Private GitHub source'}
      </p>
    </div>
    <section class="application-settings-workspace mt-4 w-full max-w-none lg:mt-0">
      <div class="grid min-w-0 gap-8 xl:grid-cols-[210px_minmax(0,1fr)] xl:gap-8">
        <aside class="application-settings-navigation in-main-nav min-w-0"></aside>
        <div class="min-w-0">
          {#if !data.has_installation && tab === 'general'}
            <div class="application-settings-form">
              <SettingsSection title="Complete GitHub installation" description="Choose which repositories this GitHub App can access before using it as a source.">
                <div class="flex flex-col items-start gap-4 sm:flex-row sm:items-center sm:justify-between">
                  <div class="flex items-start gap-3">
                    <div class="flex size-9 shrink-0 items-center justify-center rounded-lg bg-warning/10 text-warning">
                      <Reicon name="alert-triangle" class="size-4" />
                    </div>
                    <p class="max-w-xl text-[12px] leading-5 text-neutral-500 dark:text-fg-dim">
                      Repository access has not been installed yet. Complete this step before attaching the source to an application.
                    </p>
                  </div>
                  <a class="button shrink-0 button-highlighted" href={data.installation_path}>
                    Install repositories
                    {@render externalLink()}
                  </a>
                </div>
              </SettingsSection>
            </div>
          {:else if tab === 'general'}
            <form
              class="application-settings-form"
              onsubmit={(e) => {
                e.preventDefault();
                submit();
              }}
            >
              <UnsavedBar {dirty} {saving} onsave={submit} onreset={() => (form = { ...saved })} />
              <SettingsSection title="General" description="Connection and authentication settings for this private GitHub source.">
                {#snippet actions()}
                  {#if data?.is_connected}
                    <button type="button" class="button" onclick={testConnection}>Test connection</button>
                  {/if}
                  <button type="button" class="button" disabled={inputsDisabled} onclick={syncName}>
                    <Reicon name="refresh" class="size-3.5" />
                    Sync name
                  </button>
                  {#if data?.can_update}
                    <a href={data.settings_url} class="button">
                      Rename
                      {@render externalLink()}
                    </a>
                    <a href={data.installation_path} class="button">
                      Repositories
                      {@render externalLink()}
                    </a>
                  {/if}
                {/snippet}
                <div class="grid gap-4 lg:grid-cols-2">
                  <FormInput disabled={inputsDisabled} id="name" label="App name" bind:value={form.name} error={err('name')} />
                  <FormInput disabled={inputsDisabled} id="organization" label="Organization" placeholder="Personal account when empty" bind:value={form.organization} error={err('organization')} />
                  {#if data.can_make_system_wide}
                    <div class="lg:col-span-2">
                      <Listbox
                        id="isSystemWide"
                        label="Availability"
                        disabled={inputsDisabled}
                        helper="System-wide GitHub Apps can be used by every team on this Coolify instance."
                        bind:value={form.isSystemWide}
                        options={[
                          { value: false, label: 'Only this team' },
                          { value: true, label: 'Every team on this instance' },
                        ]}
                      />
                    </div>
                  {/if}
                  {#if form.isSystemWide}
                    <div class="lg:col-span-2">
                      <Callout type="warning" title="Shared with every team">Use team-specific GitHub Apps when you need repository isolation between teams.</Callout>
                    </div>
                  {/if}
                  <FormInput disabled={inputsDisabled} id="htmlUrl" label="HTML URL" bind:value={form.htmlUrl} error={err('htmlUrl')} />
                  <FormInput disabled={inputsDisabled} id="apiUrl" label="API URL" bind:value={form.apiUrl} error={err('apiUrl')} />
                  <FormInput disabled={inputsDisabled} id="customUser" label="User" required bind:value={form.customUser} error={err('customUser')} />
                  <FormInput disabled={inputsDisabled} type="number" id="customPort" label="Port" required bind:value={form.customPort} error={err('customPort')} />
                  <FormInput disabled={inputsDisabled} type="number" id="appId" label="App ID" required bind:value={form.appId} error={err('appId')} />
                  <FormInput disabled={inputsDisabled} type="number" id="installationId" label="Installation ID" required bind:value={form.installationId} error={err('installationId')} />
                  <FormInput disabled={inputsDisabled} id="clientId" label="Client ID" type="password" required bind:value={form.clientId} />
                  <FormInput disabled={inputsDisabled} id="clientSecret" label="Client secret" type="password" required bind:value={form.clientSecret} />
                  <FormInput disabled={inputsDisabled} id="webhookSecret" label="Webhook secret" type="password" required bind:value={form.webhookSecret} />
                  <Listbox
                    id="privateKeyId"
                    label="Private key"
                    required
                    disabled={inputsDisabled}
                    bind:value={() => String(form.privateKeyId), (v) => (form.privateKeyId = Number(v))}
                    options={keyOptions}
                    error={err('privateKeyId')}
                  />
                </div>
              </SettingsSection>
            </form>
          {:else if tab === 'danger'}
            <div class="application-settings-form">
              <SettingsSection id="github-app-danger-section" title="Danger zone" description="Destructive actions for this GitHub App source cannot be undone.">
                <DangerZone title="Delete GitHub App">
                  <p>
                    Permanently delete
                    <strong class="font-semibold text-black dark:text-fg">{form.name || 'this GitHub App'}</strong>
                    from Coolify. Applications using this source will need another Git provider configured.
                  </p>
                  <ul class="space-y-1 text-xs">
                    <li>• The App registration on GitHub is not removed automatically.</li>
                    <li>• Linked applications keep their Git settings until you change them.</li>
                    <li>• This source cannot be restored from Coolify after deletion.</li>
                  </ul>
                  {#snippet action()}
                    {#if data?.can_delete}
                      {@render deleteButton()}
                    {:else}
                      <button class="button button-error" disabled title="You do not have permission to delete this GitHub App.">Delete</button>
                    {/if}
                  {/snippet}
                </DangerZone>
                {#if !data.can_delete}
                  <div class="mt-4">
                    <Callout type="danger" title="Insufficient permissions">Contact a team administrator if this GitHub App must be deleted.</Callout>
                  </div>
                {/if}
              </SettingsSection>
            </div>
          {:else if tab === 'permissions'}
            <div class="application-settings-form flex flex-col gap-6">
              <SettingsSection title="Permissions" description="GitHub permissions currently granted to this App.">
                {#snippet actions()}
                  <button type="button" class="button" onclick={checkPermissions}>
                    <Reicon name="refresh" class="size-3.5" />
                    Refetch
                  </button>
                  <a href={data?.permissions_path} class="button">
                    Update on GitHub
                    {@render externalLink()}
                  </a>
                {/snippet}
                <div class="grid gap-4 lg:grid-cols-3">
                  <FormInput id="contents" helper="Read access is mandatory." label="Contents" readonly placeholder="N/A" value={data.contents ?? ''} />
                  <FormInput id="metadata" helper="Read access is mandatory." label="Metadata" readonly placeholder="N/A" value={data.metadata ?? ''} />
                  <FormInput id="pullRequests" helper="Write access is needed for preview deployment status updates." label="Pull requests" readonly placeholder="N/A" value={data.pull_requests ?? ''} />
                </div>
              </SettingsSection>
              <SettingsSection
                title="GitHub Actions runners"
                description="Your servers can run the organization's workflow jobs in Docker containers. Configure them in the GitHub Runners menu of a server."
              >
                <div class="grid gap-4 lg:grid-cols-3">
                  <FormInput id="runners" label="Self-hosted runners" readonly placeholder="N/A" value={data.organization_self_hosted_runners ?? ''} helper="Organization permission. Write access is needed to register runners." />
                  <FormInput id="actions" label="Actions" readonly placeholder="N/A" value={data.actions ?? ''} helper="Repository permission. Read access is needed for the Workflow job webhook event." />
                  <FormInput id="events" label="Webhook events" readonly placeholder="N/A" value={(data.webhook_events ?? []).join(', ')} helper="Refetch to update this list after you change the App on GitHub." />
                </div>
                {#if data.missing_runner_requirements.length > 0}
                  <Callout type="info" title="Not ready for runners" class="mt-4">
                    Update the App on GitHub, then click Refetch. Missing:
                    <ul class="mt-1 list-disc pl-4">
                      {#each data.missing_runner_requirements as requirement (requirement)}
                        <li>{requirement}</li>
                      {/each}
                    </ul>
                    <div class="mt-2">
                      <a href={data.permissions_path} class="button" target="_blank" rel="noopener noreferrer">
                        Update on GitHub
                        {@render externalLink()}
                      </a>
                    </div>
                  </Callout>
                {/if}
              </SettingsSection>
            </div>
          {:else if tab === 'resources'}
            <div class="application-settings-form">
              <SourceResources provider="GitHub" applications={data.applications} />
            </div>
          {/if}
        </div>
      </div>
    </section>
  {:else}
    <header class="mb-8 flex flex-col gap-4 sm:flex-row sm:items-start sm:justify-between">
      <div class="min-w-0">
        <h1 class="truncate text-[24px]! leading-7! font-semibold! tracking-tight!">{form.name || 'GitHub App'}</h1>
        <p class="mt-1 text-[13px] text-neutral-500 dark:text-fg-dim">Finish registering this GitHub App before using it as a source</p>
      </div>
      {#if data.can_delete}
        <div class="shrink-0">{@render deleteButton()}</div>
      {/if}
    </header>

    {#if data.can_create}
      <div class="grid gap-4 lg:grid-cols-2">
        <div class="application-settings-form flex">
          <SettingsSection class="[&>.application-settings-section-body]:flex [&>.application-settings-section-body]:flex-1 [&>.application-settings-section-body]:flex-col" title="Automated installation" description="Register through GitHub's manifest flow with permissions and webhooks preconfigured.">
            {#snippet actions()}
              <StatusBadge label="Recommended" type="success" />
            {/snippet}
            <div class="flex min-h-[24rem] flex-1 flex-col gap-4">
              <Listbox
                id="use_custom_webhook_endpoint"
                label="Webhook endpoint"
                helper="Use a custom public URL when Coolify is behind a tunnel or reverse proxy."
                bind:value={useCustom}
                options={[
                  { value: false, label: 'Use an instance endpoint' },
                  { value: true, label: 'Use a custom endpoint' },
                ]}
              />
              {#if !useCustom}
                <Listbox id="webhook_endpoint" label="Instance endpoint" bind:value={endpoint} options={endpointOptions} />
              {:else}
                <FormInput id="custom_webhook_endpoint" type="url" label="Custom endpoint" placeholder="https://coolify.example.com" helper="Do not include /webhooks." bind:value={customEndpoint} error={endpointError} />
              {/if}
              <div class="rounded-lg border border-neutral-200 bg-neutral-50 p-3 text-[12px] leading-5 text-neutral-600 dark:border-white/[0.08] dark:bg-white/[0.05] dark:text-fg-dim">
                <p class="font-medium text-black dark:text-fg">Mandatory permissions</p>
                <p class="mt-1">Contents: read · Metadata: read · Email: read</p>
              </div>
              <Listbox
                id="preview_deployment_permissions"
                label="Preview deployment access"
                helper="Write access lets Coolify post deployment status and links on pull requests."
                bind:value={previewPermissions}
                options={[
                  { value: false, label: 'Do not update pull requests' },
                  { value: true, label: 'Read and update pull requests' },
                ]}
              />
              <Listbox
                id="github_runners"
                label="GitHub Actions runners"
                disabled={!data.organization}
                helper={data.organization
                  ? 'Adds the organization Self-hosted runners (write) and Actions (read) permissions and the Workflow job webhook event.'
                  : 'Only GitHub Apps that belong to an organization can run workflow jobs. Delete this source and create a new one with an organization.'}
                bind:value={githubRunners}
                options={[
                  { value: false, label: 'Do not run workflow jobs' },
                  { value: true, label: 'Run workflow jobs on your servers' },
                ]}
              />
              <button type="button" class="button mt-auto w-full justify-center button-highlighted" onclick={register}>Register with GitHub</button>
            </div>
          </SettingsSection>
        </div>
        <div class="application-settings-form flex">
          <SettingsSection class="[&>.application-settings-section-body]:flex [&>.application-settings-section-body]:flex-1 [&>.application-settings-section-body]:flex-col" title="Manual installation" description="Enter GitHub App credentials manually for GitHub Enterprise or custom permission sets.">
            {#snippet actions()}
              <StatusBadge label="Advanced" type="neutral" />
            {/snippet}
            <div class="flex min-h-[24rem] flex-1 flex-col">
              <div class="flex size-10 items-center justify-center rounded-xl border border-neutral-200 bg-neutral-50 text-neutral-500 dark:border-white/[0.08] dark:bg-white/[0.035] dark:text-fg-dim">
                <Reicon name="settings" class="size-5" />
              </div>
              <p class="mt-4 max-w-md text-[12px] leading-5 text-neutral-500 dark:text-fg-dim">
                Use this path when the automated GitHub manifest flow is unavailable or you need complete control over the App configuration.
              </p>
              <button type="button" class="button mt-auto w-fit" onclick={manual}>
                Continue manually
                <Reicon name="arrow-right" class="size-3.5" />
              </button>
            </div>
          </SettingsSection>
        </div>
      </div>
    {:else}
      <Callout type="danger" title="Insufficient permissions">You do not have permission to create GitHub Apps. Contact your team administrator.</Callout>
    {/if}
  {/if}
{/if}

{#if confirmDelete && data}
  <ConfirmModal
    title="Confirm GitHub App Deletion?"
    actions={['The selected GitHub App will be permanently deleted.']}
    confirmationText={data.name}
    confirmationLabel="Please confirm the execution of the actions by entering the GitHub App Name below"
    shortConfirmationLabel="GitHub App Name"
    confirmWithPassword={false}
    step2ButtonText="Permanently Delete"
    onconfirm={remove}
    onclose={() => (confirmDelete = false)}
  />
{/if}
