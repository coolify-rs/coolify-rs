<script lang="ts" generics="T">
  // The steps every `Server\New\By*` component shares: the token picker
  // (components/server/provider-token-picker.blade.php), then the provider data with
  // its loading and error states; the form itself is the caller's.
  import type { Snippet } from 'svelte';
  import CloudTokenForm from './CloudTokenForm.svelte';
  import Empty from './Empty.svelte';
  import Callout from './Callout.svelte';
  import Reicon from './Reicon.svelte';
  import SettingsSection from './SettingsSection.svelte';
  import SkeletonForm from './SkeletonForm.svelte';
  import { cloudUi, type CloudForm } from '../lib/cloudApi';
  import { failure } from '../lib/serverApi';
  import { link, router } from '../lib/router.svelte';
  import { notify } from '../lib/toast.svelte';

  let {
    provider,
    providerLabel,
    routeType,
    tokenUuid,
    onready,
    loadingDescription,
    spinner = false,
    signupUrl,
    signupText,
    signupRel,
    children,
  }: {
    provider: string;
    providerLabel: string;
    routeType: string;
    tokenUuid?: string;
    /** The provider data arrived: `mount()` and `load*Data()` of the form. */
    onready: (form: CloudForm, data: T) => void;
    loadingDescription: string;
    /** Hostinger shows `x-loading` where the others show the form skeleton. */
    spinner?: boolean;
    signupUrl: string;
    signupText: string;
    signupRel?: string;
    children: Snippet<[CloudForm, () => Promise<void>]>;
  } = $props();

  let form = $state<CloudForm | null>(null);
  let loaded = $state(false);
  let dataError = $state<string | null>(null);
  let adding = $state(false);

  const selected = $derived(tokenUuid ? form?.tokens.find((t) => t.uuid === tokenUuid) : undefined);

  async function loadForm() {
    try {
      const f = await cloudUi.form(provider);
      form = f;
      if (tokenUuid && f.tokens.some((t) => t.uuid === tokenUuid)) await loadData();
    } catch (e) {
      failure(e);
    }
  }

  async function loadData() {
    try {
      const d = await cloudUi.data<T>(provider, tokenUuid as string);
      if (d.error) {
        dataError = d.error;
        notify.failure(d.error);
      } else if (form) {
        onready(form, d);
        loaded = true;
      }
    } catch (e) {
      failure(e);
    }
  }

  async function refreshKeys() {
    const f = await cloudUi.form(provider);
    if (form) form.keys = f.keys;
  }

  $effect(() => {
    loadForm();
  });

  const card =
    'group flex min-h-28 min-w-0 flex-col rounded-xl border border-neutral-200 bg-white p-3 shadow-sm transition-all hover:-translate-y-px hover:border-neutral-300 hover:no-underline hover:shadow-md dark:border-white/[0.08] dark:bg-white/[0.05] dark:hover:border-white/[0.14]';
</script>

<div class="w-full">
  {#if form && !selected}
    <div class="flex flex-col gap-6">
      <SettingsSection title="{providerLabel} account" description="Choose the cloud credential coolify-rs should use for this server." flush>
        {#if form.tokens.length === 0}
          <Empty title="No {providerLabel} tokens" description="Add an API token to continue provisioning." iconName="keys" size="sm">
            {#snippet contents()}
              <button type="button" class="button button-highlighted" onclick={() => (adding = true)}>
                <Reicon name="plus" class="size-3.5" />
                Add token
              </button>
            {/snippet}
          </Empty>
        {:else}
          <div class="grid grid-cols-1 gap-3 p-3 sm:grid-cols-2 lg:grid-cols-4">
            {#each form.tokens as token (token.id)}
              <a class={card} href="/servers/new/{routeType}/{token.uuid}" use:link>
                <div class="flex items-start gap-3">
                  <span
                    class="flex size-8 shrink-0 items-center justify-center rounded-lg border border-neutral-200 bg-neutral-50 text-neutral-500 dark:border-white/[0.08] dark:bg-white/[0.04] dark:text-fg-dim"
                  >
                    <Reicon name="keys" class="size-4" />
                  </span>
                  <div class="min-w-0 flex-1">
                    <h3 class="truncate text-[13px]! font-semibold! text-black dark:text-fg">{token.name ?? `${providerLabel} token`}</h3>
                    <p class="mt-1 line-clamp-2 text-[11px] leading-4 text-neutral-500 dark:text-fg-faint">
                      {token.description || 'Use this token to provision the server.'}
                    </p>
                  </div>
                </div>
              </a>
            {/each}
          </div>
        {/if}
      </SettingsSection>
      <p class="text-[11px] text-neutral-500 dark:text-fg-faint">
        New to {providerLabel}?
        <a href={signupUrl} target="_blank" rel={signupRel} class="font-medium text-coollabs hover:underline dark:text-brand-light">Create an account</a>
        {signupText}
      </p>
    </div>
  {:else if form}
    <div>
      {#if dataError}
        <SettingsSection title="Unable to load {providerLabel}" description="The selected token could not access the provider API.">
          <Callout type="warning" title="Provider request failed">
            <pre class="mt-2 whitespace-pre-wrap break-words text-[11px]">{dataError}</pre>
          </Callout>
          <div class="mt-4">
            <a class="button" href="/servers/new/{routeType}" use:link>Select another token</a>
          </div>
        </SettingsSection>
      {:else if loaded}
        {@render children(form, refreshKeys)}
      {:else}
        <SettingsSection title="Loading {providerLabel}" description={loadingDescription}>
          {#if spinner}
            <div class="flex min-h-40 items-center justify-center">
              <div class="inline-flex items-center justify-center gap-2 text-[13px] text-neutral-500 dark:text-fg-dim" role="status" aria-live="polite">
                <svg class="loading-indicator shrink-0 animate-spin size-4" viewBox="0 0 24 24" fill="none" aria-hidden="true">
                  <circle class="opacity-20" cx="12" cy="12" r="9" stroke="currentColor" stroke-width="2" />
                  <path class="opacity-80" d="M21 12a9 9 0 0 0-9-9" stroke="currentColor" stroke-width="2" stroke-linecap="round" />
                </svg>
                <span>Loading {providerLabel} data...</span>
              </div>
            </div>
          {:else}
            <SkeletonForm fields={6} label="Loading {providerLabel} data" />
          {/if}
        </SettingsSection>
      {/if}
    </div>
  {/if}
</div>

{#if adding}
  <CloudTokenForm
    {provider}
    {providerLabel}
    onclose={() => (adding = false)}
    oncreated={(token) => {
      // `handleTokenAdded`: selects the new token and goes on to the next step.
      adding = false;
      router.go(`/servers/new/${routeType}/${token.uuid}`);
    }}
  />
{/if}
