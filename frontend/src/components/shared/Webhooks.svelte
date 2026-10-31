<script lang="ts">
  // Port of upstream livewire/project/shared/webhooks.blade.php.
  import Callout from '../Callout.svelte';
  import CopyInput from '../CopyInput.svelte';
  import Empty from '../Empty.svelte';
  import FormInput from '../FormInput.svelte';
  import Reicon from '../Reicon.svelte';
  import SettingsSection from '../SettingsSection.svelte';
  import UnsavedBar from '../UnsavedBar.svelte';
  import { ApiError, request } from '../../lib/api';
  import { link } from '../../lib/router.svelte';
  import { resPath, type ResourceKind } from '../../lib/sharedApi';
  import { session } from '../../lib/session.svelte';
  import { notify } from '../../lib/toast.svelte';

  let { kind, uuid }: { kind: ResourceKind; uuid: string } = $props();

  const providers = [
    { name: 'GitHub', key: 'github', description: 'Accepts JSON or form-urlencoded webhook payloads.' },
    { name: 'GitLab', key: 'gitlab', description: 'Use the same secret when configuring the webhook in GitLab.' },
    { name: 'Bitbucket', key: 'bitbucket', description: 'Use the same secret when configuring the webhook in Bitbucket.' },
    { name: 'Gitea', key: 'gitea', description: 'Use the same secret when configuring the webhook in Gitea.' },
  ];

  let data = $state<any>(null);
  let secrets = $state<Record<string, string>>({});
  let saved = $state<Record<string, string>>({});
  let error = $state<ApiError | null>(null);
  let saving = $state(false);

  const base = $derived(`${resPath(kind, uuid)}/webhooks`);
  const dirty = $derived(providers.some((p) => (secrets[p.key] ?? '') !== (saved[p.key] ?? '')));

  async function load() {
    const d = await request<any>('GET', base);
    data = d;
    const s = Object.fromEntries(providers.map((p) => [p.key, d.secrets?.[p.key] ?? '']));
    secrets = { ...s };
    saved = { ...s };
  }

  $effect(() => {
    load();
  });

  async function submit() {
    if (saving) return;
    saving = true;
    error = null;
    try {
      const r = await request<{ message: string }>('POST', base, secrets);
      notify.success(r.message);
      saved = { ...secrets };
    } catch (err) {
      const f = err instanceof ApiError ? err : new ApiError(0, String(err));
      if (Object.keys(f.errors).length) error = f;
      else notify.failure(f.message);
    } finally {
      saving = false;
    }
  }

  async function generate(provider: string) {
    try {
      const r = await request<{ message: string; secret: string }>('POST', `${base}/generate`, { provider });
      secrets[provider] = r.secret;
      saved[provider] = r.secret;
      notify.success(r.message);
    } catch (err) {
      notify.failure(err instanceof ApiError ? err.message : String(err));
    }
  }
</script>

{#snippet external(href: string, text: string)}
  <a class="button" {href} target="_blank" rel="noopener noreferrer">
    {text}
    <Reicon name="external-link" class="size-3" />
  </a>
{/snippet}

{#snippet deploy()}
  {#if !data.api_enabled}
    <Callout type="warning" title="API access is turned off" class="mb-4">
      Enable API access before external services can call the deploy webhook.
      {#if session.isInstanceAdmin}
        <div class="mt-2"><a use:link href="/settings/advanced" class="button">Enable API access</a></div>
      {:else}
        Ask an instance admin to enable API access.
      {/if}
    </Callout>
  {/if}
  <CopyInput label="Deploy webhook URL" text={data.deploy_webhook} />
{/snippet}

{#if data}
  {#if data.is_application}
    <div class="flex flex-col gap-6">
      <SettingsSection id="deploy-webhook-section" title="Deploy webhook" description="Trigger a deployment from an external service. Requests must include a valid Coolify API authorization token.">
        {#snippet actions()}
          {@render external('https://coolify.io/docs/api-reference/authorization', 'Documentation')}
        {/snippet}
        {@render deploy()}
      </SettingsSection>

      {#if data.git_based && data.manual.github && data.manual.gitlab}
        <form
          onsubmit={(e) => {
            e.preventDefault();
            submit();
          }}
          class="application-settings-form flex flex-col"
        >
          <UnsavedBar {dirty} {saving} onsave={submit} onreset={() => (secrets = { ...saved })} />
          <SettingsSection id="manual-git-webhooks-section" title="Manual Git webhooks" description="Configure these endpoints when the repository is not connected through an official Git App." flush>
            {#snippet actions()}
              {#if data.git_webhook}{@render external(data.git_webhook, 'Repository settings')}{/if}
            {/snippet}
            <div class="divide-y divide-neutral-200 dark:divide-white/[0.07]">
              {#each providers as p (p.key)}
                <section class="px-4 py-5 first:pt-4 last:pb-4">
                  <div class="mb-4">
                    <h4 class="text-sm font-semibold text-black dark:text-fg">{p.name}</h4>
                    <p class="mt-1 text-[13px] leading-5 text-neutral-500 dark:text-fg-dim">{p.description}</p>
                  </div>
                  <div class="grid gap-4 md:grid-cols-2">
                    <CopyInput label="Webhook URL" text={data.manual[p.key] ?? ''} />
                    {#if data.can_update}
                      <div class="flex items-end gap-2">
                        <FormInput
                          type="password"
                          id="{p.key}ManualWebhookSecret"
                          label="Webhook secret"
                          helper="Must exactly match the secret configured in {p.name}. New secrets need at least 16 characters. Leave empty to disable this webhook."
                          autocomplete="new-password"
                          copyable
                          bind:value={secrets[p.key]}
                          error={error?.field(p.key)}
                        />
                        <button type="button" class="button" aria-label="Generate {p.name} webhook secret" onclick={() => generate(p.key)}>Generate</button>
                      </div>
                    {:else}
                      <FormInput id="{p.key}Hidden" disabled label="Webhook secret" value="Hidden (only administrators can view)" />
                    {/if}
                  </div>
                </section>
              {/each}
            </div>
          </SettingsSection>
        </form>
      {:else if data.gitlab_app_webhook}
        <SettingsSection id="manual-git-webhooks-section" title="Manual Git webhooks" description="GitLab Apps cannot register repository webhooks, so add this webhook to the GitLab project yourself." flush>
          {#snippet actions()}
            {#if data.git_webhook}{@render external(data.git_webhook, 'Repository settings')}{/if}
          {/snippet}
          <section class="px-4 py-5">
            <div class="mb-4">
              <h4 class="text-sm font-semibold text-black dark:text-fg">GitLab</h4>
              <p class="mt-1 text-[13px] leading-5 text-neutral-500 dark:text-fg-dim">
                Add this URL in the GitLab project under Settings → Webhooks. Set the Secret token to the Webhook secret token from the source settings, and enable Push events and Merge request events.
              </p>
            </div>
            <CopyInput label="Webhook URL" text={data.gitlab_app_webhook} />
          </section>
        </SettingsSection>
      {:else}
        <SettingsSection id="manual-git-webhooks-section" title="Manual Git webhooks" description="Manual repository webhooks are only required when a repository is not connected through an official Git App.">
          <Empty size="sm" title="Managed by your Git App" description="This application uses an official Git App, so Coolify configures repository webhooks automatically." iconName="notifications" />
        </SettingsSection>
      {/if}
    </div>
  {:else}
    <div class="application-settings-form">
      <SettingsSection title="Deploy webhook" description="Trigger an external deployment with a valid Coolify API authorization token.">
        {#snippet actions()}
          {@render external('https://coolify.io/docs/api-reference/authorization', 'Documentation')}
        {/snippet}
        {@render deploy()}
      </SettingsSection>
    </div>
  {/if}
{/if}
