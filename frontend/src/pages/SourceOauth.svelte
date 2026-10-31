<script lang="ts">
  import { untrack } from 'svelte';
  // Port of upstream livewire/source/{gitlab,gitea}/change.blade.php
  // (+ Source\Gitlab\Change, Source\Gitea\Change).
  import Callout from '../components/Callout.svelte';
  import Checkbox from '../components/Checkbox.svelte';
  import ConfirmModal from '../components/ConfirmModal.svelte';
  import FormInput from '../components/FormInput.svelte';
  import Listbox from '../components/Listbox.svelte';
  import SettingsSection from '../components/SettingsSection.svelte';
  import SettingsSubsection from '../components/SettingsSubsection.svelte';
  import SourceResources from '../components/SourceResources.svelte';
  import StatusBadge from '../components/StatusBadge.svelte';
  import UnsavedBar from '../components/UnsavedBar.svelte';
  import { ApiError } from '../lib/api';
  import { router } from '../lib/router.svelte';
  import { notify } from '../lib/toast.svelte';
  import { setSourceCrumbs, sourcesUi, type OauthSource } from '../lib/sourcesApi';

  let { provider, uuid }: { provider: 'gitlab' | 'gitea'; uuid: string } = $props();

  const gitlab = $derived(provider === 'gitlab');
  const name_ = $derived(gitlab ? 'GitLab' : 'Gitea');
  const redirectPath = $derived(`/webhooks/source/${provider}/redirect`);

  type Form = {
    name: string;
    clientId: string;
    clientSecret: string;
    webhookSecret: string;
    organization: string;
    htmlUrl: string;
    apiUrl: string;
    customUser: string;
    customPort: string;
    isSystemWide: boolean;
    privateKeyId: string;
  };
  const blank: Form = {
    name: '',
    clientId: '',
    clientSecret: '',
    webhookSecret: '',
    organization: '',
    htmlUrl: '',
    apiUrl: '',
    customUser: '',
    customPort: '',
    isSystemWide: false,
    privateKeyId: '',
  };
  let data = $state<OauthSource | null>(null);
  let form = $state<Form>({ ...blank });
  let saved = $state<Form>({ ...blank });
  let errors = $state<Record<string, string[]>>({});
  let saving = $state(false);
  let confirmDelete = $state(false);
  let advancedOpen = $state(false);
  let useCustom = $state(false);
  let endpoint = $state('');
  let customEndpoint = $state('');
  const dirty = $derived(JSON.stringify(form) !== JSON.stringify(saved));
  const disabled = $derived(!data?.can_update);

  const known = $derived.by(() => {
    const e = data?.endpoints;
    const list = [e?.fqdn, e?.ipv4, e?.ipv6, e?.app_url].filter((v): v is string => !!v);
    return [...new Set(list)];
  });
  const base = $derived(((useCustom && customEndpoint ? customEndpoint : endpoint) || '').replace(/\/+$/, ''));
  const redirectUri = $derived(base ? base + redirectPath : '');
  const oauthUrl = $derived.by(() => {
    if (!data) return '';
    const query = new URLSearchParams({
      client_id: form.clientId,
      redirect_uri: redirectUri,
      response_type: 'code',
      scope: gitlab ? 'api read_user read_repository' : 'read:user read:repository read:organization',
      state: data.oauth_state,
    });
    return `${form.htmlUrl.replace(/\/+$/, '')}/${gitlab ? 'oauth/authorize' : 'login/oauth/authorize'}?${query}`;
  });

  function apply(d: OauthSource) {
    data = d;
    const next: Form = {
      name: d.name,
      clientId: d.client_id ?? '',
      clientSecret: d.client_secret ?? '',
      webhookSecret: d.webhook_secret ?? '',
      organization: d.organization ?? '',
      htmlUrl: d.html_url,
      apiUrl: d.api_url,
      customUser: d.custom_user,
      customPort: String(d.custom_port),
      isSystemWide: d.is_system_wide,
      privateKeyId: d.private_key_id == null ? '' : String(d.private_key_id),
    };
    form = { ...next };
    saved = { ...next };
    const e = d.endpoints;
    endpoint = e.fqdn ?? e.ipv4 ?? e.ipv6 ?? e.app_url ?? '';
    useCustom = false;
    if (d.redirect_uri) {
      const savedBase = d.redirect_uri.split(redirectPath)[0].replace(/\/+$/, '');
      const list = [e.fqdn, e.ipv4, e.ipv6, e.app_url].filter(Boolean).map((u) => String(u).replace(/\/+$/, ''));
      if (list.includes(savedBase)) endpoint = savedBase;
      else if (savedBase) {
        useCustom = true;
        customEndpoint = savedBase;
      }
    }
    document.title = `${d.name || `${name_} App`} | Sources | coolify-rs`;
    setSourceCrumbs(provider, d.uuid, d.is_connected);
  }

  async function load() {
    apply(await sourcesUi.oauth(provider, uuid));
  }

  $effect(() => {
    load().catch((e) => notify.failure(e instanceof ApiError ? e.message : String(e)));
    const flash = router.query('error');
    // `session('error')`, once: the toast list must not become a dependency of this effect.
    if (flash) untrack(() => notify.failure(flash));
  });

  /** `persistRedirectUriFromEndpoint()`. */
  function persistRedirect() {
    if (data?.can_update && redirectUri) sourcesUi.persistRedirect(provider, uuid, redirectUri).catch(() => {});
  }

  function deriveApi() {
    const expected = saved.htmlUrl.replace(/\/+$/, '') + (gitlab ? '/api/v4' : '/api/v1');
    if (!form.apiUrl || form.apiUrl === expected) form.apiUrl = form.htmlUrl.replace(/\/+$/, '') + (gitlab ? '/api/v4' : '/api/v1');
  }

  async function submit() {
    if (saving) return;
    saving = true;
    errors = {};
    try {
      const r = await sourcesUi.updateOauth(provider, uuid, {
        name: form.name,
        api_url: form.apiUrl,
        html_url: form.htmlUrl,
        custom_user: form.customUser,
        custom_port: form.customPort,
        client_id: form.clientId,
        client_secret: form.clientSecret,
        webhook_secret: form.webhookSecret,
        organization: form.organization,
        is_system_wide: form.isSystemWide,
        private_key_id: form.privateKeyId === '' ? null : Number(form.privateKeyId),
        redirect_uri: redirectUri,
      });
      notify.success(r.message);
      await load();
    } catch (e) {
      if (e instanceof ApiError) {
        errors = e.errors;
        notify.failure(e.message);
      } else notify.failure(String(e));
    } finally {
      saving = false;
    }
  }

  async function testConnection() {
    try {
      notify.success((await sourcesUi.test(provider, uuid)).message);
    } catch (e) {
      notify.failure(e instanceof ApiError ? e.message : String(e));
    }
  }

  async function remove(): Promise<string | void> {
    try {
      await sourcesUi.remove(provider, uuid);
      router.go('/sources');
    } catch (e) {
      notify.failure(e instanceof ApiError ? e.message : String(e));
      confirmDelete = false;
    }
  }

  const err = (k: string) => errors[k]?.[0];
  const keyOptions = $derived([{ value: '', label: 'None' }, ...(data?.private_keys ?? []).map((k) => ({ value: String(k.id), label: k.name }))]);
  const createUrl = $derived(gitlab ? `${form.htmlUrl.replace(/\/+$/, '')}/-/profile/applications` : `${form.htmlUrl.replace(/\/+$/, '')}/user/settings/applications`);
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
  {#if data.is_connected}
    <header class="mb-5 flex flex-col gap-4 sm:flex-row sm:items-start sm:justify-between">
      <div class="min-w-0">
        <div class="flex flex-wrap items-center gap-2">
          <h1 class="truncate text-[24px]! leading-7! font-semibold! tracking-tight!">{form.name || `${name_} App`}</h1>
          <StatusBadge label="Connected" type="success" />
        </div>
        <p class="mt-1 text-[13px] text-neutral-500 dark:text-fg-dim">
          {saved.organization ? `${name_} App for ${saved.organization}` : `Private ${name_} source`}
        </p>
      </div>
      <div class="flex shrink-0 flex-wrap items-center gap-2 sm:ml-auto">
        <button type="button" class="button" onclick={testConnection}>Test connection</button>
        {#if data.can_delete}
          {@render deleteButton()}
        {/if}
      </div>
    </header>

    <form
      class="application-settings-form flex flex-col gap-6"
      onsubmit={(e) => {
        e.preventDefault();
        submit();
      }}
    >
      <UnsavedBar {dirty} {saving} onsave={submit} onreset={() => (form = { ...saved })} />
      <SettingsSection title="General" description="Name, visibility, and webhook of this source.">
        <div class="grid gap-4 lg:grid-cols-2">
          <FormInput {disabled} id="name" label="Name" bind:value={form.name} error={err('name')} />
          {#if data.can_make_system_wide || !gitlab}
            <div class="lg:col-span-2 max-w-xs">
              <Checkbox id="isSystemWide" {disabled} label="System wide" helper="If checked, this {name_} App will be available for everyone in this Coolify instance." bind:checked={form.isSystemWide} />
            </div>
          {/if}
          {#if form.isSystemWide}
            <div class="lg:col-span-2">
              <Callout type="warning" title="Shared with every team">
                System-wide {name_} Apps are shared across all teams on this Coolify instance. Prefer team-specific apps when you need repository isolation.
              </Callout>
            </div>
          {/if}
        </div>
        <SettingsSubsection
          title="Webhook"
          helper={gitlab
            ? 'Configure this webhook URL in your GitLab project settings (Settings → Webhooks). On self-managed GitLab, an admin can instead add the same URL and token once as a system hook (Admin Area → System hooks) with Push events and Merge request events enabled.'
            : 'Configure this webhook URL in Gitea (user, organization, or repository settings → Webhooks → Add webhook → Gitea).'}
        >
          <div class="grid gap-4 lg:grid-cols-2">
            <div class="lg:col-span-2">
              <FormInput id="webhookUrl" readonly label="Webhook URL" value="{base}/webhooks/source/{provider}/events" />
            </div>
            {#if gitlab}
              <FormInput {disabled} id="webhookToken" label="Webhook secret token" type="password" helper="Set this same token in your GitLab webhook's Secret token field. Stored encrypted." bind:value={form.webhookSecret} />
            {:else}
              <FormInput
                {disabled}
                id="webhookSecret"
                label="Webhook secret"
                type="password"
                helper="Set this as the Secret in a Gitea webhook (user, organization, or repository settings → Webhooks → Add webhook → Gitea), content type application/json, trigger on Push and Pull Request events. Stored encrypted."
                bind:value={form.webhookSecret}
              />
            {/if}
          </div>
        </SettingsSubsection>
      </SettingsSection>

      <SettingsSection id="connection-section" title="Connection" description="OAuth credentials and the endpoints Coolify uses to reach this source.">
        <SettingsSubsection first title="OAuth credentials" helper={gitlab ? 'Application credentials from your GitLab OAuth application.' : 'Client credentials from your Gitea OAuth2 application.'}>
          <div class="grid gap-4 lg:grid-cols-2">
            <FormInput {disabled} id="clientId" label={gitlab ? 'Application ID' : 'Client ID'} bind:value={form.clientId} />
            <FormInput {disabled} id="clientSecretInput" label={gitlab ? 'Application secret' : 'Client secret'} type="password" helper="Stored encrypted. Leave empty to keep the existing secret." bind:value={form.clientSecret} />
            {#if gitlab}
              <FormInput {disabled} id="groupName" label="Group name" helper="Comma-separated group names to filter visible repositories." bind:value={form.organization} />
            {:else}
              <FormInput {disabled} id="organization" label="Organization" helper="Comma-separated organization or user names to filter visible repositories." bind:value={form.organization} />
            {/if}
          </div>
        </SettingsSubsection>
        <SettingsSubsection title="Self-hosted / advanced" helper="Override endpoints and SSH settings for self-hosted {name_}.">
          <div class="grid gap-4 lg:grid-cols-2">
            <FormInput {disabled} id="htmlUrl" label="{name_} URL" bind:value={form.htmlUrl} onchange={deriveApi} error={err('htmlUrl')} />
            <FormInput {disabled} id="apiUrl" label="API URL" bind:value={form.apiUrl} error={err('apiUrl')} />
            <FormInput {disabled} id="customUser" label="SSH user" bind:value={form.customUser} error={err('customUser')} />
            <FormInput {disabled} type="number" id="customPort" label="SSH port" bind:value={form.customPort} error={err('customPort')} />
            <div class="lg:col-span-2">
              <Listbox id="privateKeyId" label="SSH private key (optional)" {disabled} bind:value={form.privateKeyId} options={keyOptions} error={err('privateKeyId')} />
            </div>
          </div>
        </SettingsSubsection>
      </SettingsSection>
    </form>

    <div class="application-settings-form mt-6">
      <SourceResources provider={name_} applications={data.applications} repositoryColumn />
    </div>
  {:else}
    <header class="mb-8 flex flex-col gap-4 sm:flex-row sm:items-start sm:justify-between">
      <div class="min-w-0">
        <h1 class="truncate text-[24px]! leading-7! font-semibold! tracking-tight!">{form.name || `${name_} App`}</h1>
        <p class="mt-1 text-[13px] text-neutral-500 dark:text-fg-dim">Finish connecting this {name_} App before using it as a source</p>
      </div>
      {#if data.can_delete}
        <div class="shrink-0 sm:ml-auto">{@render deleteButton()}</div>
      {/if}
    </header>

    <div class="application-settings-form flex flex-col gap-6">
      <SettingsSection
        title="Step 1 · Create an OAuth application"
        description={gitlab
          ? 'Register a new OAuth application in your GitLab instance with the redirect URI below.'
          : 'In Gitea, open Manage OAuth2 Applications and create a new application with the redirect URI below.'}
      >
        <div class="flex flex-col gap-3 text-[12px] leading-5 text-neutral-600 dark:text-fg-dim">
          <a href={createUrl} target="_blank" class="inline-flex w-fit items-center gap-1 font-medium text-black underline-offset-2 hover:underline dark:text-fg">
            {createUrl}
            {@render externalLink()}
          </a>
          <ul class="list-inside list-disc space-y-1.5">
            <li>
              Set <strong class="text-black dark:text-fg">Redirect URI</strong> to:
              <code class="rounded bg-neutral-100 px-1.5 py-0.5 text-[11px] dark:bg-white/[0.06]">{redirectUri}</code>
            </li>
            {#if gitlab}
              <li>
                Enable scopes: <code class="rounded bg-neutral-100 px-1.5 py-0.5 text-[11px] dark:bg-white/[0.06]">api</code>,
                <code class="rounded bg-neutral-100 px-1.5 py-0.5 text-[11px] dark:bg-white/[0.06]">read_user</code>,
                <code class="rounded bg-neutral-100 px-1.5 py-0.5 text-[11px] dark:bg-white/[0.06]">read_repository</code>
              </li>
              <li>Uncheck <strong class="text-black dark:text-fg">Confidential</strong> if you run into issues</li>
            {:else}
              <li>Keep <strong class="text-black dark:text-fg">Confidential client</strong> checked</li>
            {/if}
          </ul>
        </div>
      </SettingsSection>

      <form
        class="contents"
        onsubmit={(e) => {
          e.preventDefault();
          submit();
        }}
      >
        <SettingsSection title="Step 2 · Enter credentials" description={gitlab ? 'Paste the Application ID and secret from GitLab, then save.' : 'Paste the Client ID and Client secret from Gitea, then save.'}>
          {#snippet actions()}
            <button type="submit" class="button">Save</button>
          {/snippet}
          <div class="grid gap-4 lg:grid-cols-2">
            <FormInput id="name" label="Name" bind:value={form.name} error={err('name')} />
            <FormInput id="clientId" label={gitlab ? 'Application ID' : 'Client ID'} required helper={gitlab ? 'The Application ID from your GitLab OAuth Application.' : 'The Client ID from your Gitea OAuth2 application.'} bind:value={form.clientId} />
            <FormInput
              id="clientSecretInput"
              label={gitlab ? 'Application secret' : 'Client secret'}
              type="password"
              required={!form.clientSecret && !data.has_client_secret}
              helper={gitlab ? 'The Secret from your GitLab OAuth Application. Saved encrypted.' : 'The Client secret from your Gitea OAuth2 application. Saved encrypted.'}
              bind:value={form.clientSecret}
            />
            {#if gitlab}
              <FormInput id="groupName" label="Group name" helper="Optional. Comma-separated group names to filter repositories." bind:value={form.organization} />
            {:else}
              <FormInput id="organization" label="Organization" helper="Optional. Comma-separated organization or user names to filter repositories." bind:value={form.organization} />
            {/if}
          </div>

          <div class="mt-4 grid gap-4 lg:grid-cols-2">
            <div class="lg:col-span-2 text-[12px] text-neutral-500 dark:text-fg-dim">
              {gitlab
                ? 'GitLab will redirect back to this Coolify URL. It must match the Callback URL on your GitLab OAuth Application exactly.'
                : 'Gitea will redirect back to this Coolify URL. It must match the Redirect URI on your Gitea OAuth2 application exactly.'}
            </div>
            <div class="lg:col-span-2 max-w-md">
              <Listbox
                id="use_custom_webhook_endpoint"
                label="Webhook endpoint"
                helper="Use a custom public URL when Coolify is behind a tunnel or reverse proxy."
                bind:value={useCustom}
                onchange={persistRedirect}
                options={[
                  { value: false, label: 'Use an instance endpoint' },
                  { value: true, label: 'Use a custom endpoint' },
                ]}
              />
            </div>
            {#if !useCustom}
              <div class="lg:col-span-2">
                <Listbox id="webhook_endpoint" label="Selected endpoint" helper="{name_} will use this endpoint unless custom mode is enabled." bind:value={endpoint} onchange={persistRedirect} options={known.map((v) => ({ value: v, label: `Use ${v}` }))} />
              </div>
            {:else}
              <div class="lg:col-span-2">
                <FormInput id="custom_webhook_endpoint" type="url" label="Custom endpoint" placeholder="https://coolify.example.com" helper="{name_} will use this custom public URL. Do not include /webhooks." bind:value={customEndpoint} onchange={persistRedirect} />
              </div>
            {/if}
          </div>

          <div class="mt-4">
            <button
              type="button"
              onclick={() => (advancedOpen = !advancedOpen)}
              class="flex w-full items-center justify-between rounded-lg border border-neutral-200 px-3 py-2.5 text-left text-[12px] font-medium text-neutral-700 transition-colors hover:bg-neutral-50 dark:border-white/[0.08] dark:text-fg-dim dark:hover:bg-white/[0.03]"
            >
              Advanced / self-hosted
              <svg class="size-3.5 transition-transform {advancedOpen ? 'rotate-180' : ''}" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
                <polyline points="6 9 12 15 18 9"></polyline>
              </svg>
            </button>
            {#if advancedOpen || err('htmlUrl') || err('apiUrl')}
              <div class="mt-3 grid gap-4 rounded-lg border border-neutral-200 p-3 lg:grid-cols-2 dark:border-white/[0.08]">
                <FormInput id="htmlUrl" label="{name_} URL" helper="Only change this for self-hosted {name_} (e.g. https://{provider}.example.com)." bind:value={form.htmlUrl} onchange={deriveApi} error={err('htmlUrl')} />
                <FormInput id="apiUrl" label="API URL" helper="Usually your {name_} URL with {gitlab ? '/api/v4' : '/api/v1'} appended." bind:value={form.apiUrl} error={err('apiUrl')} />
                <FormInput id="customUser" label="SSH user" bind:value={form.customUser} />
                <FormInput type="number" id="customPort" label="SSH port" bind:value={form.customPort} />
                {#if data.can_make_system_wide || !gitlab}
                  <div class="max-w-xs lg:col-span-2">
                    <Checkbox id="isSystemWide" label="System wide" helper="If checked, this {name_} App will be available for everyone in this Coolify instance." bind:checked={form.isSystemWide} />
                  </div>
                {/if}
              </div>
            {/if}
          </div>
        </SettingsSection>
      </form>

      {#if saved.clientId}
        <SettingsSection title="Step 3 · Authorize with {name_}" description={gitlab
            ? 'Authorize Coolify with your GitLab instance. The redirect URI must match the Callback URL configured in GitLab.'
            : 'Authorize Coolify with your Gitea instance. The redirect URI must match the one configured in Gitea.'}>
          <a href={oauthUrl} class="button button-highlighted">
            Connect to {name_}
            {@render externalLink()}
          </a>
        </SettingsSection>
      {/if}
    </div>
  {/if}
{/if}

{#if confirmDelete && data}
  <ConfirmModal
    title="Confirm {name_} App Deletion?"
    actions={[`The selected ${name_} App will be permanently deleted.`]}
    confirmationText={data.name}
    confirmationLabel="Please confirm by entering the {name_} App Name below"
    shortConfirmationLabel="{name_} App Name"
    confirmWithPassword={false}
    step2ButtonText="Permanently Delete"
    onconfirm={remove}
    onclose={() => (confirmDelete = false)}
  />
{/if}
