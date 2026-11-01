<script lang="ts">
  // Port of upstream livewire/project/application/previews.blade.php and preview/form.blade.php
  // (Project\Application\Previews, Preview\Form).
  import { untrack } from 'svelte';
  import Callout from '../Callout.svelte';
  import ConfirmModal from '../ConfirmModal.svelte';
  import DatabaseStatusSummary from '../DatabaseStatusSummary.svelte';
  import Empty from '../Empty.svelte';
  import FormInput from '../FormInput.svelte';
  import Listbox from '../Listbox.svelte';
  import Modal from '../Modal.svelte';
  import Reicon from '../Reicon.svelte';
  import SettingsSection from '../SettingsSection.svelte';
  import SkeletonList from '../SkeletonList.svelte';
  import UnsavedBar from '../UnsavedBar.svelte';
  import PreviewDomains from './PreviewDomains.svelte';
  import { ApiError } from '../../lib/api';
  import { previewsUi, type DeployAnswer, type PreviewRow, type PreviewsPage, type PullRequest } from '../../lib/previewsApi';
  import { link, router } from '../../lib/router.svelte';
  import { notify } from '../../lib/toast.svelte';

  let { uuid, base, onchange }: { uuid: string; base: string; onchange?: () => void } = $props();

  let data = $state<PreviewsPage | null>(null);
  let template = $state('');
  let savedTemplate = $state('');
  let saving = $state(false);
  let prsOpen = $state(false);
  let prsLoading = $state(false);
  let pullRequests = $state<PullRequest[]>([]);
  let rateLimit = $state<string | number | null>(null);
  let manualId = $state('');
  let manualTag = $state('');
  let manualError = $state('');
  let tags = $state<Record<number, string>>({});
  // The open menu: `${kind}-${pull_request_id}`.
  let menu = $state<string | null>(null);
  let stopping = $state<PreviewRow | null>(null);
  let deleting = $state<PreviewRow | null>(null);

  const message = (e: unknown) => (e instanceof ApiError ? e.message : String(e));
  const canUpdate = $derived(!!data?.can_update);

  async function load() {
    const next = await previewsUi.page(uuid);
    data = next;
    tags = Object.fromEntries(next.previews.map((p) => [p.id, p.docker_registry_image_tag ?? '']));
  }

  $effect(() => {
    untrack(() =>
      load().then(() => {
        template = data?.preview_url_template ?? '';
        savedTemplate = template;
      }),
    );
    const close = () => (menu = null);
    window.addEventListener('click', close);
    return () => window.removeEventListener('click', close);
  });

  async function run(call: () => Promise<{ message?: string }>) {
    try {
      const r = await call();
      if (r.message) notify.success(r.message);
    } catch (e) {
      notify.failure(message(e));
    }
    await load().catch(() => {});
    onchange?.();
  }

  /** The end of `deploy()`: a toast, or the deployment's page. */
  async function runDeploy(call: () => Promise<DeployAnswer>) {
    try {
      const r = await call();
      if (r.deployment_uuid) {
        router.go(`${base}/deployment/${r.deployment_uuid}`);
        return;
      }
      if (r.toast === 'error') notify.error(r.title ?? 'Error', r.message);
      else if (r.toast === 'success') notify.ok(r.title ?? 'Success', r.message);
    } catch (e) {
      notify.failure(message(e));
    }
    await load().catch(() => {});
  }

  async function submitTemplate() {
    saving = true;
    try {
      const r = await previewsUi.template(uuid, template);
      notify.success(r.message);
      template = r.preview_url_template;
      savedTemplate = template;
    } catch (e) {
      notify.failure(message(e));
    } finally {
      saving = false;
    }
  }

  async function resetTemplate() {
    try {
      const r = await previewsUi.resetTemplate(uuid);
      notify.success(r.message);
      template = r.preview_url_template;
      savedTemplate = template;
    } catch (e) {
      notify.failure(message(e));
    }
  }

  async function loadPrs() {
    prsLoading = true;
    try {
      const r = await previewsUi.pullRequests(uuid);
      pullRequests = r.pull_requests;
      rateLimit = r.rate_limit_remaining;
    } catch (e) {
      rateLimit = 0;
      notify.failure(message(e));
    } finally {
      prsLoading = false;
    }
  }

  async function submitManual() {
    manualError = '';
    try {
      const r = await previewsUi.manual(uuid, manualId, manualTag);
      manualId = '';
      manualTag = '';
      if (r.deployment_uuid) {
        router.go(`${base}/deployment/${r.deployment_uuid}`);
        return;
      }
      if (r.toast === 'error') notify.error(r.title ?? 'Error', r.message);
      else if (r.toast === 'success') notify.ok(r.title ?? 'Success', r.message);
      await load();
    } catch (e) {
      if (e instanceof ApiError && e.errors.pull_request_id?.length) manualError = e.errors.pull_request_id[0];
      else notify.failure(message(e));
    }
  }

  const toggleMenu = (e: MouseEvent, name: string) => {
    e.stopPropagation();
    menu = menu === name ? null : name;
  };
  const stopped = (p: PreviewRow) => (p.status ?? '').startsWith('exited');
</script>

{#snippet chevron()}
  <Reicon name="chevron-down" class="size-3 opacity-55" />
{/snippet}

{#if data}
  <div class="flex flex-col gap-6">
    {#if data.git_based}
      <SettingsSection id="preview-settings-section" title="Preview settings" description="Automatic pull request deployments and who can trigger them.">
        {#snippet actions()}
          {#if canUpdate}
            {#if data?.is_github_app_source}
              <button type="button" class="button" onclick={() => (prsOpen = true)}>Load pull requests</button>
            {/if}
            {#if data?.enabled}
              <button type="button" class="button" onclick={() => run(() => previewsUi.settings(uuid, { enabled: false }))}>Disable preview deployments</button>
            {:else}
              <button type="button" class="button button-highlighted" onclick={() => run(() => previewsUi.settings(uuid, { enabled: true }))}>Enable preview deployments</button>
            {/if}
          {/if}
        {/snippet}

        <div class="w-full">
          <Listbox
            id="isPrDeploymentsPublicEnabled"
            label="PR deployment access"
            helper="When public, anyone can trigger PR deployments. Otherwise only PRs from branches in the same repository are deployed and fork PRs are blocked.<br><br><span class='font-bold'>Secrets:</span> the code of a fork PR can read all environment variables of its preview. When public, previews do not get preview variables that still have the production value. Give previews their own values, and do not use production secrets in them."
            value={data.public_enabled}
            disabled={!canUpdate || !data.enabled}
            options={[
              { value: false, label: 'Repository members only' },
              { value: true, label: 'Public (fork PRs allowed)' },
            ]}
            onchange={(v) => run(() => previewsUi.settings(uuid, { public_enabled: v }))}
          />
        </div>
      </SettingsSection>
    {/if}

    <form
      class="application-settings-form flex flex-col"
      onsubmit={(e) => {
        e.preventDefault();
        submitTemplate();
      }}
    >
      <UnsavedBar dirty={template !== savedTemplate} {saving} onsave={submitTemplate} onreset={() => (template = savedTemplate)} />
      <SettingsSection id="preview-template-section" title="Preview URL template" description="Define how Coolify generates domains for pull request deployments.">
        {#snippet actions()}
          {#if canUpdate}
            <button type="button" class="button" onclick={resetTemplate}>Reset template</button>
          {/if}
        {/snippet}

        <FormInput
          id="previewUrlTemplate"
          label="URL template"
          disabled={!canUpdate}
          helper={'Use @{{ random }} for a random subdomain, @{{ pr_id }} for the pull request number, or @{{ domain }} for the application domain.'}
          bind:value={template}
        />

        {#if template}
          <div class="mt-4 flex items-center justify-between gap-3 rounded-lg bg-neutral-100 px-3 py-2.5 ring-1 ring-neutral-200 dark:bg-white/[0.04] dark:ring-white/[0.07]">
            <span class="text-[13px] text-neutral-500 dark:text-fg-dim">Generated pattern</span>
            <code class="break-all text-right font-mono text-xs text-neutral-700 dark:text-fg">{savedTemplate}</code>
          </div>
        {/if}
      </SettingsSection>
    </form>

    {#if data.has_additional_servers}
      <Callout type="info" title="Preview deployment server">Preview deployments run on {data.server_name}.</Callout>
    {/if}

    {#if data.is_docker_image}
      <SettingsSection id="manual-preview-section" title="Manual preview" description="Deploy a preview directly from a Docker image tag.">
        <form
          class="grid gap-4 md:grid-cols-[minmax(0,1fr)_minmax(0,1fr)_auto] md:items-end"
          onsubmit={(e) => {
            e.preventDefault();
            submitManual();
          }}
        >
          <FormInput id="manualPullRequestId" label="Preview ID" helper="Used for domains, logs, container names, and cleanup." error={manualError} bind:value={manualId} />
          <FormInput id="manualDockerTag" label="Docker tag" helper="For example, pr_1234." bind:value={manualTag} />
          {#if canUpdate}
            <button type="submit" class="button">Deploy preview</button>
          {/if}
        </form>
      </SettingsSection>
    {/if}

    <SettingsSection id="preview-deployments-section" title="Preview deployments" description="Manage domains, deployments, logs, and lifecycle actions for configured previews." flush>
      {#each data.previews as preview (preview.pull_request_id)}
        {@const pr = preview.pull_request_id}
        {@const isStopped = stopped(preview)}
        <section class="border-b border-neutral-200 p-4 last:border-b-0 dark:border-white/[0.07]">
          <div class="flex flex-col gap-3 lg:flex-row lg:items-start lg:justify-between">
            <div class="flex min-w-0 items-center gap-3">
              <div class="flex size-9 shrink-0 items-center justify-center rounded-lg bg-neutral-100 font-mono text-xs font-semibold text-neutral-600 ring-1 ring-neutral-200 dark:bg-white/[0.05] dark:text-fg-dim dark:ring-white/[0.07]">
                #{pr}
              </div>
              <div class="min-w-0">
                <div class="flex flex-wrap items-center gap-2">
                  <h4 class="text-sm font-semibold text-black dark:text-fg">Preview #{pr}</h4>
                  <DatabaseStatusSummary status={preview.status ?? ''} title="Preview status" restartLimit={preview.restart_limit} />
                </div>
              </div>
            </div>

            <div id="preview-header-controls-{pr}" class="flex shrink-0 flex-wrap items-center gap-2 lg:justify-end">
              <div class="relative">
                <button type="button" class="button gap-1.5" title="Preview links" aria-expanded={menu === `links-${pr}`} aria-haspopup="menu" onclick={(e) => toggleMenu(e, `links-${pr}`)}>
                  <Reicon name="external-link" class="size-3.5 opacity-70" />
                  Links
                  {@render chevron()}
                </button>
                {#if menu === `links-${pr}`}
                  <div class="listbox-panel top-full! right-0! left-auto! z-[90]! mt-1! w-56! min-w-56!" role="menu">
                    {#if !isStopped && preview.fqdn}
                      <a target="_blank" rel="noreferrer" title="Open preview in a new tab" class="listbox-option justify-start! gap-2.5!" href={preview.fqdn} role="menuitem">
                        <Reicon name="external-link" class="size-3.5 opacity-70" />
                        <span class="min-w-0 truncate">Open preview</span>
                      </a>
                    {/if}
                    {#if preview.pull_request_html_url}
                      <a target="_blank" rel="noreferrer" title="Open pull request in a new tab" class="listbox-option justify-start! gap-2.5!" href={preview.pull_request_html_url} role="menuitem">
                        <Reicon name="external-link" class="size-3.5 opacity-70" />
                        <span class="min-w-0 truncate">Open pull request</span>
                      </a>
                    {/if}
                  </div>
                {/if}
              </div>

              <div class="relative">
                <button type="button" class="button gap-1.5" title="Preview logs" aria-expanded={menu === `logs-${pr}`} aria-haspopup="menu" onclick={(e) => toggleMenu(e, `logs-${pr}`)}>
                  <Reicon name="browser-terminal" class="size-3.5 opacity-70" />
                  Logs
                  {@render chevron()}
                </button>
                {#if menu === `logs-${pr}`}
                  <div class="listbox-panel top-full! right-0! left-auto! z-[90]! mt-1! w-44! min-w-44!" role="menu">
                    <a use:link class="listbox-option justify-start! gap-2.5!" href="{base}/deployment?pull_request_id={pr}" role="menuitem">
                      <Reicon name="graph" class="size-3.5 opacity-70" />
                      Deployment logs
                    </a>
                    <a use:link class="listbox-option justify-start! gap-2.5!" href="{base}/logs?pull_request_id={pr}" role="menuitem">
                      <Reicon name="browser-terminal" class="size-3.5 opacity-70" />
                      Runtime logs
                    </a>
                  </div>
                {/if}
              </div>

              <div class="relative">
                <button type="button" class="button gap-1.5" title="Preview actions" aria-expanded={menu === `actions-${pr}`} aria-haspopup="menu" onclick={(e) => toggleMenu(e, `actions-${pr}`)}>
                  Actions
                  <span class="inline-flex transition-transform" class:rotate-180={menu === `actions-${pr}`}>
                    {@render chevron()}
                  </span>
                </button>
                {#if menu === `actions-${pr}`}
                  <div class="listbox-panel top-full! right-0! left-auto! z-[90]! mt-1! w-52! min-w-52!" role="menu">
                    {#if canUpdate}
                      <button type="button" class="listbox-option justify-start! gap-2.5!" role="menuitem" onclick={() => runDeploy(() => previewsUi.rebuild(uuid, pr))}>
                        <Reicon name="refresh" class="size-3.5 opacity-70" />
                        Rebuild
                      </button>
                      <button
                        type="button"
                        class="listbox-option justify-start! gap-2.5!"
                        role="menuitem"
                        onclick={() => runDeploy(() => previewsUi.deploy(uuid, { pull_request_id: pr, tag: preview.docker_registry_image_tag ?? '' }))}
                      >
                        <Reicon name="play-circle" class="size-3.5 opacity-70" />
                        {isStopped ? 'Deploy' : 'Redeploy'}
                      </button>
                      {#if !isStopped}
                        <button type="button" class="listbox-option justify-start! gap-2.5! text-error!" role="menuitem" onclick={() => (stopping = preview)}>
                          <Reicon name="stop" class="size-3.5" />
                          Stop
                        </button>
                      {/if}
                      <button type="button" class="listbox-option justify-start! gap-2.5! text-error!" role="menuitem" onclick={() => (deleting = preview)}>
                        <Reicon name="trash" class="size-3.5" />
                        Delete
                      </button>
                    {/if}
                  </div>
                {/if}
              </div>
            </div>
          </div>

          <div class="mt-4 border-t border-neutral-200 pt-4 dark:border-white/[0.07]">
            <PreviewDomains {uuid} page={data} {preview} onchange={() => load()} />

            {#if data.is_docker_image}
              <form
                class="application-settings-section-body is-flush mt-3 overflow-visible"
                onsubmit={(e) => {
                  e.preventDefault();
                  run(() => previewsUi.saveTag(uuid, pr, tags[preview.id] ?? ''));
                }}
              >
                <div class="data-table-header grid-cols-1"><span>Docker tag</span></div>
                <div class="p-3">
                  <FormInput id="previewDockerTags-{preview.id}" disabled={!canUpdate} bind:value={tags[preview.id]} onchange={() => run(() => previewsUi.saveTag(uuid, pr, tags[preview.id] ?? ''))} />
                </div>
              </form>
            {/if}
          </div>
        </section>
      {:else}
        <Empty title="No preview deployments" description="Configure a pull request or manual preview to create an isolated deployment." iconName="eye" />
      {/each}
    </SettingsSection>
  </div>

  {#if prsOpen}
    <Modal title="Pull requests" subtitle="Load open pull requests from GitHub, then configure or deploy a preview." isLarge onclose={() => (prsOpen = false)}>
      <div class="mb-4 flex items-center justify-end gap-2">
        {#if rateLimit !== null}
          <span class="text-xs text-neutral-500 dark:text-fg-dim">{rateLimit} requests remaining</span>
        {/if}
        <button type="button" class="button" onclick={loadPrs}>{rateLimit === null ? 'Load pull requests' : 'Refresh'}</button>
      </div>
      <div class="-m-4">
        {#if prsLoading}
          <SkeletonList rows={3} label="Loading pull requests" />
        {:else}
          {#each pullRequests as pull (pull.number)}
            <div class="flex flex-col gap-3 border-b border-neutral-200 px-4 py-3.5 last:border-b-0 sm:flex-row sm:items-center dark:border-white/[0.07]">
              <div class="flex size-9 shrink-0 items-center justify-center rounded-lg bg-neutral-100 font-mono text-xs font-semibold text-neutral-600 ring-1 ring-neutral-200 dark:bg-white/[0.05] dark:text-fg-dim dark:ring-white/[0.07]">
                #{pull.number}
              </div>
              <div class="min-w-0 flex-1">
                <h4 class="truncate text-sm font-semibold text-black dark:text-fg">{pull.title}</h4>
                <a target="_blank" rel="noreferrer" class="mt-1 inline-flex items-center gap-1 text-xs text-neutral-500 hover:text-coollabs dark:text-fg-dim dark:hover:text-brand-light" href={pull.html_url}>
                  Open on GitHub
                  <Reicon name="external-link" class="size-3" />
                </a>
              </div>
              <div class="flex shrink-0 items-center gap-2">
                <button type="button" class="button" onclick={() => run(() => previewsUi.add(uuid, pull.number, pull.html_url))}>Configure</button>
                <button type="button" class="button" onclick={() => runDeploy(() => previewsUi.deploy(uuid, { pull_request_id: pull.number, html_url: pull.html_url, add: true }))}>Deploy preview</button>
              </div>
            </div>
          {:else}
            <Empty size="sm" title="No open pull requests" description="No open pull requests were found for this repository." iconName="sources" />
          {/each}
        {/if}
      </div>
    </Modal>
  {/if}

  {#if stopping}
    {@const preview = stopping}
    <ConfirmModal
      title="Stop preview deployment?"
      actions={['This preview deployment will be stopped.', 'All non-persistent preview data will be removed.']}
      confirmWithText={false}
      confirmWithPassword={false}
      step2ButtonText="Stop preview deployment"
      onconfirm={async () => {
        await run(() => previewsUi.stop(uuid, preview.pull_request_id));
      }}
      onclose={() => (stopping = null)}
    />
  {/if}

  {#if deleting}
    {@const preview = deleting}
    <ConfirmModal
      title="Delete preview deployment?"
      actions={['All containers for this preview deployment will be stopped and permanently deleted.']}
      confirmationText="{preview.fqdn ?? ''}/"
      confirmationLabel="Enter the preview deployment name to confirm deletion"
      shortConfirmationLabel="Preview deployment name"
      confirmWithPassword={false}
      onconfirm={async () => {
        await run(() => previewsUi.remove(uuid, preview.pull_request_id));
      }}
      onclose={() => (deleting = null)}
    />
  {/if}
{/if}
