<script lang="ts">
  // Port of upstream livewire/project/application/source.blade.php (+
  // Project\Application\Source): repository, branch and commit, the deploy key
  // and the Git provider App.
  import ConfirmModal from './ConfirmModal.svelte';
  import Empty from './Empty.svelte';
  import FormInput from './FormInput.svelte';
  import Reicon from './Reicon.svelte';
  import SettingsSection from './SettingsSection.svelte';
  import StatusBadge from './StatusBadge.svelte';
  import UnsavedBar from './UnsavedBar.svelte';
  import { appUi, ApiError, type ApplicationSource, type SourceCard } from '../lib/api';
  import { link } from '../lib/router.svelte';
  import { notify } from '../lib/toast.svelte';

  let { uuid, onsaved }: { uuid: string; onsaved: () => Promise<void> } = $props();

  type Form = { gitRepository: string; gitBranch: string; gitCommitSha: string };
  let data = $state<ApplicationSource | null>(null);
  let form = $state<Form>({ gitRepository: '', gitBranch: '', gitCommitSha: '' });
  let saved = $state<Form>({ gitRepository: '', gitBranch: '', gitCommitSha: '' });
  let saving = $state(false);
  let changing = $state<SourceCard | null>(null);
  const dirty = $derived(JSON.stringify(form) !== JSON.stringify(saved));
  const canUpdate = $derived(data?.can_update ?? false);

  const GITHUB = 'App\\Models\\GithubApp';
  const GITLAB = 'App\\Models\\GitlabApp';
  const GITEA = 'App\\Models\\GiteaApp';

  async function load() {
    const d = await appUi.source(uuid);
    data = d;
    const next = { gitRepository: d.git_repository ?? '', gitBranch: d.git_branch ?? '', gitCommitSha: d.git_commit_sha ?? '' };
    form = { ...next };
    saved = { ...next };
  }

  $effect(() => {
    load().catch((e) => notify.failure(e instanceof ApiError ? e.message : String(e)));
  });

  /** `submit()`. */
  async function submit() {
    if (!canUpdate || saving) return;
    saving = true;
    try {
      const r = await appUi.updateSource(uuid, {
        git_repository: form.gitRepository,
        git_branch: form.gitBranch,
        git_commit_sha: form.gitCommitSha,
      });
      notify.success(r.message);
      await load();
      await onsaved();
    } catch (e) {
      notify.failure(e instanceof ApiError ? e.message : String(e));
    } finally {
      saving = false;
    }
  }

  /** `setPrivateKey()`. */
  async function setPrivateKey(id: number) {
    try {
      const r = await appUi.changeSource(uuid, { private_key_id: id });
      notify.success(r.message);
      await load();
      await onsaved();
    } catch (e) {
      notify.failure(e instanceof ApiError ? e.message : String(e));
    }
  }

  /** `changeSource()`. */
  async function changeSource(card: SourceCard): Promise<string | void> {
    try {
      const r = await appUi.changeSource(uuid, { source_id: card.id, source_type: card.type });
      notify.success(r.message);
    } catch (e) {
      notify.failure(e instanceof ApiError ? e.message : String(e));
    }
    changing = null;
    await load();
    await onsaved();
  }
</script>

{#snippet externalLink()}
  <svg class="inline-flex w-3 h-3 dark:text-neutral-400 text-black" fill="none" stroke="currentColor" viewBox="0 0 24 24">
    <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M10 6H6a2 2 0 00-2 2v10a2 2 0 002 2h10a2 2 0 002-2v-4M14 4h6m0 0v6m0-6L10 14" />
  </svg>
{/snippet}

{#snippet gitIcon(git: string)}
  <!-- components/git-icon.blade.php -->
  <svg xmlns="http://www.w3.org/2000/svg" class="w-6 h-6 size-4" viewBox="0 0 24 24" stroke-width="1.5" stroke="currentColor" fill="none" stroke-linecap="round" stroke-linejoin="round">
    <path stroke="none" d="M0 0h24v24H0z" fill="none" />
    {#if git === GITHUB}
      <path
        d="M9 19c-4.3 1.4 -4.3 -2.5 -6 -3m12 5v-3.5c0 -1 .1 -1.4 -.5 -2c2.8 -.3 5.5 -1.4 5.5 -6a4.6 4.6 0 0 0 -1.3 -3.2a4.2 4.2 0 0 0 -.1 -3.2s-1.1 -.3 -3.5 1.3a12.3 12.3 0 0 0 -6.2 0c-2.4 -1.6 -3.5 -1.3 -3.5 -1.3a4.2 4.2 0 0 0 -.1 3.2a4.6 4.6 0 0 0 -1.3 3.2c0 4.6 2.7 5.7 5.5 6c-.6 .6 -.6 1.2 -.5 2v3.5"
      />
    {:else if git === GITLAB}
      <path d="M21 14l-9 7l-9 -7l3 -11l3 7h6l3 -7z" />
    {:else if git === GITEA}
      <path d="M3 7h14v5a5 5 0 0 1 -5 5h-4a5 5 0 0 1 -5 -5z" />
      <path d="M17 9h1.5a2.5 2.5 0 0 1 0 5h-1.5" />
      <path d="M9 9.5v4" />
      <path d="M9 11.5l2.5 -1.5" />
      <path d="M5 20h10" />
    {:else}
      <path d="M16 12m-1 0a1 1 0 1 0 2 0a1 1 0 1 0 -2 0" />
      <path d="M12 8m-1 0a1 1 0 1 0 2 0a1 1 0 1 0 -2 0" />
      <path d="M12 16m-1 0a1 1 0 1 0 2 0a1 1 0 1 0 -2 0" />
      <path d="M12 15v-6" />
      <path d="M15 11l-2 -2" />
      <path d="M11 7l-1.9 -1.9" />
      <path
        d="M13.446 2.6l7.955 7.954a2.045 2.045 0 0 1 0 2.892l-7.955 7.955a2.045 2.045 0 0 1 -2.892 0l-7.955 -7.955a2.045 2.045 0 0 1 0 -2.892l7.955 -7.955a2.045 2.045 0 0 1 2.892 0z"
      />
    {/if}
  </svg>
{/snippet}

{#if data}
  <div class="application-settings-form flex flex-col gap-6">
    <form
      class="flex flex-col gap-6"
      onsubmit={(e) => {
        e.preventDefault();
        submit();
      }}
    >
      <UnsavedBar {dirty} {saving} onsave={submit} onreset={() => (form = { ...saved })} />

      <SettingsSection title="Repository" description="Configure the Git repository, branch, and commit coolify-rs deploys.">
        {#snippet actions()}
          <div class="flex flex-wrap items-center gap-2">
            <a target="_blank" class="button" href={data?.branch_location ?? ''}>
              Repository
              {@render externalLink()}
            </a>
            {#if data?.installation_path}
              <a target="_blank" class="button" href={data.installation_path}>
                Git app
                {@render externalLink()}
              </a>
            {/if}
            <a target="_blank" class="button" href={data?.commits_url ?? ''}>
              Commits
              {@render externalLink()}
            </a>
          </div>
        {/snippet}

        {#if data.private_key_id == null}
          <div
            class="mb-4 flex items-center justify-between gap-3 rounded-lg bg-neutral-50 px-3 py-2.5 ring-1 ring-neutral-200 dark:bg-white/[0.05] dark:ring-white/[0.07]"
          >
            <span class="text-[12px] text-neutral-500 dark:text-fg-dim">Connected source</span>
            <span class="text-[12px] font-medium text-neutral-900 dark:text-fg">
              {data.current_source?.name ?? 'No source connected'}
            </span>
          </div>
        {/if}

        <div class="grid gap-4 lg:grid-cols-2">
          <FormInput placeholder="coollabsio/coolify-example" id="gitRepository" label="Repository" disabled={!canUpdate} bind:value={form.gitRepository} />
          <FormInput placeholder="main" id="gitBranch" label="Branch" disabled={!canUpdate} bind:value={form.gitBranch} />
          <FormInput placeholder="HEAD" id="gitCommitSha" label="Commit SHA" disabled={!canUpdate} bind:value={form.gitCommitSha} />
        </div>
      </SettingsSection>
    </form>

    {#if data.private_key_id != null}
      <SettingsSection title="Deploy key" description="The SSH key coolify-rs uses to clone this private repository.">
        <div
          class="mb-4 flex items-center justify-between gap-3 rounded-lg bg-neutral-50 px-3 py-2.5 ring-1 ring-neutral-200 dark:bg-white/[0.05] dark:ring-white/[0.07]"
        >
          <span class="text-[12px] text-neutral-500 dark:text-fg-dim">Attached private key</span>
          <span class="text-[12px] font-medium text-neutral-900 dark:text-fg">{data.private_key_name ?? ''}</span>
        </div>
        {#if canUpdate}
          <div class="grid gap-2 sm:grid-cols-2">
            {#each data.private_keys as key (key.id)}
              <button
                type="button"
                onclick={() => setPrivateKey(key.id)}
                class="flex min-h-11 items-center rounded-lg border border-neutral-200 bg-white px-3 text-left text-[12px] font-medium transition-colors hover:bg-neutral-50 dark:border-white/[0.08] dark:bg-white/[0.05] dark:hover:bg-white/[0.05]"
              >
                {key.name}
              </button>
            {:else}
              <Empty title="No alternative private keys" description="Add another private key from Keys & Tokens before switching." iconName="keys" size="sm" />
            {/each}
          </div>
        {/if}
      </SettingsSection>
    {:else if canUpdate}
      <SettingsSection title="Git source" description="Switch the Git provider App coolify-rs uses to clone this repository.">
        <div class="grid gap-2 sm:grid-cols-2">
          {#if data.current_source}
            <div
              class="relative flex min-h-16 items-center gap-3 rounded-xl border border-coollabs/30 bg-coollabs/[0.06] p-3 ring-1 ring-coollabs/20 dark:border-brand-light/30 dark:bg-brand/[0.12] dark:ring-brand-light/15"
              aria-current="true"
            >
              <div
                class="flex size-9 shrink-0 items-center justify-center rounded-lg border border-coollabs/20 bg-white text-coollabs dark:border-brand-light/20 dark:bg-white/[0.04] dark:text-brand-light"
              >
                {@render gitIcon(data.current_source.type)}
              </div>
              <div class="min-w-0 flex-1">
                <div class="flex min-w-0 flex-wrap items-center gap-2">
                  <span class="truncate text-[13px] font-semibold text-black dark:text-fg">{data.current_source.name}</span>
                  <StatusBadge label="Current" type="success" />
                </div>
                <p class="mt-0.5 truncate text-[11px] text-neutral-500 dark:text-fg-faint">{data.current_source.subtitle}</p>
              </div>
              <Reicon name="check" class="size-4 shrink-0 text-black dark:text-fg" />
            </div>
          {/if}

          {#each data.sources as source (`${source.type}-${source.id}`)}
            <div class="relative w-full h-full">
              <button
                type="button"
                onclick={() => (changing = source)}
                class="group flex h-full w-full min-h-16 items-center gap-3 rounded-xl border border-neutral-200 bg-white p-3 text-left shadow-sm transition-all hover:-translate-y-px hover:border-neutral-300 hover:bg-neutral-50 hover:shadow-md focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-accent dark:border-white/[0.08] dark:bg-white/[0.05] dark:hover:border-white/[0.14] dark:hover:bg-white/[0.05]"
              >
                <div
                  class="flex size-9 shrink-0 items-center justify-center rounded-lg border border-neutral-200 bg-neutral-50 text-neutral-500 transition-colors group-hover:border-neutral-300 group-hover:text-neutral-700 dark:border-white/[0.08] dark:bg-white/[0.04] dark:text-fg-dim dark:group-hover:border-white/[0.12] dark:group-hover:text-fg"
                >
                  {@render gitIcon(source.type)}
                </div>
                <div class="min-w-0 flex-1">
                  <span class="block truncate text-[13px] font-semibold text-black dark:text-fg">{source.name}</span>
                  <span class="mt-0.5 block truncate text-[11px] text-neutral-500 dark:text-fg-faint">{source.subtitle}</span>
                </div>
                <Reicon
                  name="arrow-right"
                  class="size-3.5 shrink-0 text-neutral-300 opacity-0 transition-all group-hover:translate-x-0.5 group-hover:opacity-100 dark:text-fg-faint"
                />
              </button>
            </div>
          {:else}
            {#if !data.current_source}
              <div class="col-span-full">
                <Empty title="No other sources" description="Connect another Git source before moving this application." iconName="sources" size="sm">
                  {#snippet contents()}
                    <a href="/sources" use:link class="button">Connect source</a>
                  {/snippet}
                </Empty>
              </div>
            {/if}
          {/each}
        </div>
        {#if data.current_source && data.sources.length === 0}
          <p class="mt-3 text-[12px] leading-5 text-neutral-500 dark:text-fg-dim">
            No other sources available.
            <a href="/sources" use:link class="font-medium text-coollabs underline-offset-2 hover:underline dark:text-brand-light">Connect another source</a>
            to switch providers.
          </p>
        {/if}
      </SettingsSection>
    {/if}
  </div>
{/if}

{#if changing}
  {@const card = changing}
  <ConfirmModal
    title="Change Git source?"
    actions={['Change Git source to ' + card.name]}
    confirmationText="Change Git Source"
    confirmationLabel="Enter the text below to confirm changing the Git source."
    shortConfirmationLabel="Confirmation text"
    onconfirm={() => changeSource(card)}
    onclose={() => (changing = null)}
  />
{/if}
