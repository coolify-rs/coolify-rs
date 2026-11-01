<script lang="ts">
  // Port of upstream livewire/source/{github,gitlab,gitea}/create.blade.php (+ the
  // Source\{Github,Gitlab,Gitea}\Create components).
  import Callout from './Callout.svelte';
  import Checkbox from './Checkbox.svelte';
  import FormInput from './FormInput.svelte';
  import { ApiError } from '../lib/api';
  import { router } from '../lib/router.svelte';
  import { notify } from '../lib/toast.svelte';
  import { sourcesUi, type Provider } from '../lib/sourcesApi';

  let {
    provider,
    randomName,
    canCreate,
    canMakeSystemWide,
  }: { provider: Provider; randomName: string; canCreate: boolean; canMakeSystemWide: boolean } = $props();

  const meta = $derived(
    {
      github: {
        name: 'GitHub',
        intro: 'Connect a GitHub App for private repositories, webhooks, and commit / pull request deployments.',
        html: 'https://github.com',
        api: 'https://api.github.com',
        org: 'organization',
        selfHosted: 'Self-hosted / Enterprise GitHub',
        urlLabel: 'HTML URL',
        urlHelper: 'For GitHub Enterprise, enter your instance URL (e.g. https://github.example.com).',
        apiHelper: 'Usually https://api.github.com or your Enterprise API base URL.',
      },
      gitlab: {
        name: 'GitLab',
        intro: 'Connect a GitLab OAuth application for private repositories, webhooks, and deployments.',
        html: 'https://gitlab.com',
        api: 'https://gitlab.com/api/v4',
        org: 'group_name',
        selfHosted: 'Self-hosted GitLab',
        urlLabel: 'GitLab URL',
        urlHelper: 'For self-hosted GitLab, enter your instance URL (e.g. https://gitlab.example.com).',
        apiHelper: 'Usually your GitLab URL with /api/v4 appended.',
      },
      gitea: {
        name: 'Gitea',
        intro: 'Connect a Gitea OAuth2 application for private repositories, webhooks, and deployments.',
        html: 'https://gitea.com',
        api: 'https://gitea.com/api/v1',
        org: 'organization',
        selfHosted: 'Self-hosted Gitea',
        urlLabel: 'Gitea URL',
        urlHelper: 'For self-hosted Gitea, enter your instance URL (e.g. https://gitea.example.com).',
        apiHelper: 'Usually your Gitea URL with /api/v1 appended.',
      },
    }[provider],
  );

  let name = $state(randomName);
  let organization = $state('');
  let htmlUrl = $state('');
  let apiUrl = $state('');
  let customUser = $state('git');
  let customPort = $state('22');
  let isSystemWide = $state(false);
  let usePreviews = $state(true);
  let useRunners = $state(false);
  let open = $state(false);
  let busy = $state(false);
  let errors = $state<Record<string, string[]>>({});

  $effect(() => {
    if (!htmlUrl) htmlUrl = meta.html;
    if (!apiUrl) apiUrl = meta.api;
  });

  /** `updatedHtmlUrl()`. */
  let previousDerived = '';
  function derive(url: string): string {
    if (provider === 'github') {
      try {
        const u = new URL(url);
        const host = u.hostname.toLowerCase();
        if (host === 'github.com') return 'https://api.github.com';
        if (host.endsWith('.ghe.com') && !host.startsWith('api.')) return `https://api.${host}`;
        return `${u.protocol}//${u.host}/api/v3`;
      } catch {
        return apiUrl;
      }
    }
    return url.replace(/\/+$/, '') + (provider === 'gitlab' ? '/api/v4' : '/api/v1');
  }
  function onHtmlFocus() {
    previousDerived = derive(htmlUrl);
  }
  function onHtmlChange() {
    if (!apiUrl || apiUrl === previousDerived) apiUrl = derive(htmlUrl);
  }

  async function submit(e: SubmitEvent) {
    e.preventDefault();
    if (busy) return;
    busy = true;
    errors = {};
    try {
      const body: Record<string, unknown> = {
        name,
        [meta.org]: organization,
        api_url: apiUrl,
        html_url: htmlUrl,
        custom_user: customUser,
        custom_port: customPort,
        is_system_wide: isSystemWide,
      };
      if (provider === 'github') {
        body.use_for_pull_request_previews = usePreviews;
        body.use_for_github_runners = useRunners;
      }
      const r = await sourcesUi.create(provider, body);
      const query = provider === 'github' ? `?previews=${r.previews}&runners=${r.runners}` : '';
      router.go(`/source/${provider}/${r.uuid}${query}`);
    } catch (err) {
      if (err instanceof ApiError) {
        errors = err.errors;
        if (Object.keys(err.errors).length === 0) notify.failure(err.message);
      } else notify.failure(String(err));
      busy = false;
    }
  }

  const err = (k: string) => errors[k]?.[0];
</script>

{#if canCreate}
  <form onsubmit={submit} class="flex w-full flex-col gap-4">
    <p class="text-[12px] leading-5 text-neutral-500 dark:text-fg-dim">{meta.intro}</p>

    <div class="grid gap-4 sm:grid-cols-2">
      <FormInput id="name" label="Name" required bind:value={name} error={err('name')} />
      {#if provider === 'github'}
        <FormInput
          id="organization"
          label="Organization"
          required={useRunners}
          helper={useRunners ? 'GitHub Actions runners are registered at organization level.' : 'If empty, your GitHub user will be used. GitHub Actions runners need an organization.'}
          placeholder={useRunners ? 'GitHub organization, for example coollabsio' : 'Personal account when empty'}
          bind:value={organization}
          error={err('organization')}
        />
      {:else if provider === 'gitlab'}
        <FormInput
          id="group_name"
          label="Group name"
          helper="Optional. Comma-separated group names to filter repositories (e.g. myorg,myteam)."
          placeholder="All accessible repositories when empty"
          bind:value={organization}
        />
      {:else}
        <FormInput
          id="organization"
          label="Organization"
          helper="Optional. Comma-separated organization or user names to filter repositories (e.g. myorg,myuser)."
          placeholder="All accessible repositories when empty"
          bind:value={organization}
        />
      {/if}
    </div>

    {#if canMakeSystemWide || provider === 'gitea'}
      <div>
        <div class="max-w-xs">
          <Checkbox
            id="is_system_wide"
            label="System wide"
            helper="If checked, this {meta.name} {provider === 'github' ? 'App' : 'App'} will be available for everyone in this coolify-rs instance."
            bind:checked={isSystemWide}
          />
        </div>
        {#if isSystemWide}
          <div class="mt-3">
            <Callout type="warning" title="Shared with every team">
              System-wide {meta.name} Apps are available to every team on this instance. Prefer team-specific apps when you need repository isolation.
            </Callout>
          </div>
        {/if}
      </div>
    {/if}

    {#if provider === 'github'}
      <div>
        <p class="text-[12px] font-medium text-neutral-700 dark:text-fg-dim">Use this App for</p>
        <p class="mt-1 text-[12px] leading-5 text-neutral-500 dark:text-fg-dim">
          Deploying applications from private repositories is always included. The choices below select the permissions and webhook events on the next page.
        </p>
        <div class="mt-2 flex max-w-xs flex-col gap-1">
          <Checkbox id="use_for_pull_request_previews" label="Pull request previews" helper="Deploys pull requests and posts deployment status on them." bind:checked={usePreviews} />
          <Checkbox id="use_for_github_runners" label="GitHub Actions runners" helper="Runs workflow jobs on your servers. Needs an organization." bind:checked={useRunners} />
        </div>
      </div>
    {/if}

    <div class="rounded-lg border border-neutral-200 dark:border-white/[0.08]">
      <button
        type="button"
        onclick={() => (open = !open)}
        class="flex w-full items-center justify-between px-3 py-2.5 text-left text-[12px] font-medium text-neutral-700 transition-colors hover:bg-neutral-50 dark:text-fg-dim dark:hover:bg-white/[0.03]"
      >
        {meta.selfHosted}
        <svg class="size-3.5 transition-transform {open ? 'rotate-180' : ''}" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
          <polyline points="6 9 12 15 18 9"></polyline>
        </svg>
      </button>
      {#if open || Object.keys(errors).some((k) => ['html_url', 'api_url', 'custom_user', 'custom_port'].includes(k))}
        <div class="border-t border-neutral-200 px-3 py-3 dark:border-white/[0.08]">
          <div class="grid gap-4 sm:grid-cols-2">
            <FormInput id="html_url" label={meta.urlLabel} required helper={meta.urlHelper} bind:value={htmlUrl} onfocus={onHtmlFocus} onchange={onHtmlChange} error={err('html_url')} />
            <FormInput id="api_url" label="API URL" required helper={meta.apiHelper} bind:value={apiUrl} error={err('api_url')} />
            <FormInput id="custom_user" label="Custom Git user" required bind:value={customUser} error={err('custom_user')} />
            <FormInput id="custom_port" type="number" label="Custom Git port" required bind:value={customPort} error={err('custom_port')} />
          </div>
        </div>
      {/if}
    </div>

    <button class="button mt-1 w-full justify-center" type="submit" disabled={busy}>Continue</button>
  </form>
{:else}
  <Callout type="danger" title="Insufficient permissions">You don't have permission to create new {meta.name} Apps. Contact your team administrator.</Callout>
{/if}
