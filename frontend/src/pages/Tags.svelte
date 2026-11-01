<script lang="ts">
  // Port of upstream livewire/tags/show.blade.php with livewire/tags/partials/new-tag-form.blade.php.
  import Reicon from '../components/Reicon.svelte';
  import Empty from '../components/Empty.svelte';
  import Modal from '../components/Modal.svelte';
  import Dropdown from '../components/Dropdown.svelte';
  import Pager from '../components/Pager.svelte';
  import ViewToggle from '../components/ViewToggle.svelte';
  import FormInput from '../components/FormInput.svelte';
  import ConfirmModal from '../components/ConfirmModal.svelte';
  import SettingsSection from '../components/SettingsSection.svelte';
  import StatusBadge from '../components/StatusBadge.svelte';
  import ResourceLogo from '../components/ResourceLogo.svelte';
  import { ApiError } from '../lib/api';
  import { tagsUi, type Connectable, type TagDetails, type TagRow } from '../lib/tagsApi';
  import { listView } from '../lib/listview.svelte';
  import { link, router } from '../lib/router.svelte';
  import { notify } from '../lib/toast.svelte';
  import { page as pageState } from '../lib/page.svelte';

  let { tagName }: { tagName?: string } = $props();

  document.title = 'Tags | coolify-rs';

  let tags = $state<TagRow[] | null>(null);
  let canCreate = $state(false);
  let details = $state<TagDetails | null>(null);

  async function load() {
    const list = await tagsUi.index();
    tags = list.tags;
    canCreate = list.can_create;
    if (tagName) {
      const d = await tagsUi.show(tagName);
      if (!d.found) {
        router.go('/tags', true);
        return;
      }
      details = d;
      pageState.resourceSwitcher = {
        title: 'Tags',
        label: d.tag.name,
        items: [
          { label: 'All tags', href: '/tags', active: false },
          ...list.tags.map((t) => ({ label: t.name, href: `/tags/${encodeURIComponent(t.name)}`, active: t.name === d.tag.name })),
        ],
      };
    }
  }
  load();

  // `wire:poll="getDeployments"`.
  $effect(() => {
    if (!tagName) return;
    const timer = setInterval(async () => {
      const d = await tagsUi.show(tagName!);
      if (d.found && details) details.deployments = d.deployments;
    }, 2000);
    return () => clearInterval(timer);
  });

  const count = $derived(tags?.length ?? 0);
  const resourceCount = $derived((details?.applications.length ?? 0) + (details?.services.length ?? 0));

  // New tag.
  let creating = $state(false);
  let newName = $state('');

  async function createTag(e: SubmitEvent) {
    e.preventDefault();
    try {
      const r = await tagsUi.create(newName);
      router.go(`/tags/${encodeURIComponent(r.name)}`);
    } catch (err) {
      // The modal is `wire:ignore`, so upstream's validation errors show nowhere.
      const error = err instanceof ApiError ? err : new ApiError(0, String(err));
      if (!Object.keys(error.errors).length) notify.failure(error.message);
    }
  }

  // List.
  const view = listView('tags', 'grid');
  let search = $state('');
  let sortBy = $state('name-asc');
  let page = $state(1);
  let pageSize = $state(12);
  const sortOptions = [
    { value: 'name-asc', label: 'Name A–Z' },
    { value: 'name-desc', label: 'Name Z–A' },
    { value: 'resources', label: 'Most resources' },
  ];
  const resources = (t: TagRow) => t.applications_count + t.services_count;
  const filtered = $derived.by(() => {
    const q = search.trim().toLowerCase();
    const list = (tags ?? []).filter((t) => !q || t.name.toLowerCase().includes(q));
    return [...list].sort((a, b) => {
      if (sortBy === 'name-desc') return b.name.localeCompare(a.name);
      if (sortBy === 'resources') return resources(b) - resources(a) || a.name.localeCompare(b.name);
      return a.name.localeCompare(b.name);
    });
  });
  const paginated = $derived(filtered.slice((page - 1) * pageSize, page * pageSize));

  // Connect modal.
  let connectOpen = $state(false);
  let connectable = $state<Connectable[] | null>(null);
  const groups = $derived.by(() => {
    const m = new Map<string, Connectable[]>();
    for (const r of connectable ?? []) m.set(r.context, [...(m.get(r.context) ?? []), r]);
    return [...m.entries()].sort(([a], [b]) => a.localeCompare(b, undefined, { numeric: true, sensitivity: 'base' }));
  });

  async function openConnect() {
    connectOpen = true;
    connectable = null;
    connectable = (await tagsUi.connectable(tagName!)).resources;
  }

  async function toggleResource(r: Connectable) {
    try {
      await tagsUi.toggle(tagName!, r.uuid);
      details = await tagsUi.show(tagName!);
      connectable = (await tagsUi.connectable(tagName!)).resources;
    } catch (e) {
      notify.failure(e instanceof ApiError ? e.message : String(e));
    }
  }

  // Confirmations.
  let deleting = $state(false);
  let redeploying = $state(false);

  async function deleteTag() {
    try {
      await tagsUi.destroy(tagName!);
      router.go('/tags');
    } catch (e) {
      notify.failure(e instanceof ApiError ? e.message : String(e));
    }
  }

  async function redeployAll() {
    try {
      const r = await tagsUi.redeploy(tagName!);
      notify.success(r.message);
      details = await tagsUi.show(tagName!);
    } catch (e) {
      notify.failure(e instanceof ApiError ? e.message : String(e));
    }
    redeploying = false;
  }

  const headline = (s: string) => s.replace(/_/g, ' ').replace(/\b\w/g, (c) => c.toUpperCase());
</script>

<div class="application-settings-form w-full">
  <header class="mb-5 flex items-start justify-between gap-4 {tagName ? 'lg:hidden' : ''}">
      <div class="min-w-0">
        <h1 class="truncate text-[24px]! leading-7! font-semibold! tracking-tight!">Tags</h1>
        <p class="mt-1 text-[13px] text-neutral-500 dark:text-fg-dim">
          {#if !tags}
            &nbsp;
          {:else if tags.length === 0}
            Group applications and services for bulk deploys
          {:else if details}
            Manage webhook deploys and resources for this tag
          {:else}
            {count} {count === 1 ? 'tag' : 'tags'} for bulk deploys and grouping
          {/if}
        </p>
      </div>
      {#if !tagName && canCreate}
        <div class="flex shrink-0 items-center gap-2">
          <button type="button" class="button button-highlighted whitespace-nowrap" onclick={() => (creating = true)}>
            <Reicon name="plus" class="size-3.5" />
            New tag
          </button>
        </div>
      {/if}
    </header>

  {#if tags}
    {#if tags.length === 0}
      <Empty title="No tags yet" description="Create a tag, then connect it to resources to group related deployments." iconName="tags" />
    {:else if !details}
      <div class="w-full">
        <div class="mb-3 flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
          <div class="relative w-full sm:max-w-sm">
            <Reicon name="search" class="pointer-events-none absolute top-1/2 left-2.5 z-10 size-3.5 -translate-y-1/2 text-neutral-400 dark:text-fg-faint" />
            <input
              bind:value={search}
              oninput={() => (page = 1)}
              type="search"
              placeholder="Search tags"
              class="h-8! w-full rounded-lg! border-neutral-200! bg-white! py-0! pr-8! pl-8! text-[12px]! shadow-none! placeholder:text-neutral-400 focus:border-accent! focus:ring-0! dark:border-white/[0.08]! dark:bg-white/[0.035]! dark:text-fg! dark:placeholder:text-fg-faint"
            />
            {#if search}
              <button
                onclick={() => ((search = ''), (page = 1))}
                type="button"
                class="absolute top-1/2 right-2 flex size-5 -translate-y-1/2 items-center justify-center rounded text-neutral-400 transition-colors hover:bg-neutral-100 hover:text-black dark:text-fg-faint dark:hover:bg-white/[0.07] dark:hover:text-fg"
                aria-label="Clear search"
              >
                <span class="text-sm leading-none">×</span>
              </button>
            {/if}
          </div>

          <div class="flex flex-wrap items-center gap-2">
            <Dropdown panelClass="w-52!">
              {#snippet trigger({ open })}
                <button type="button" class="button" aria-haspopup="listbox" aria-expanded={open}>
                  <svg class="size-3.5 opacity-65" viewBox="0 0 24 24" fill="none" aria-hidden="true">
                    <path d="M8 5v14m0 0-3-3m3 3 3-3M16 19V5m0 0-3 3m3-3 3 3" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" />
                  </svg>
                  Sort
                </button>
              {/snippet}
              {#snippet children({ close })}
                {#each sortOptions as option (option.value)}
                  <button
                    type="button"
                    class="flex h-9 w-full items-center rounded-md px-2 text-left text-[12px] text-neutral-600 transition-colors hover:bg-neutral-100 hover:text-black dark:text-fg-dim dark:hover:bg-white/[0.06] dark:hover:text-fg"
                    onclick={() => {
                      sortBy = option.value;
                      close();
                      page = 1;
                    }}
                  >
                    <span class="flex-1">{option.label}</span>
                    {#if sortBy === option.value}
                      <svg class="size-3.5 text-black dark:text-fg" viewBox="0 0 12 12" fill="none" aria-hidden="true">
                        <path d="m2.5 6.25 2.1 2.1 4.9-5" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round" />
                      </svg>
                    {/if}
                  </button>
                {/each}
              {/snippet}
            </Dropdown>

            <ViewToggle canvasUrl="/canvas/tags" bind:mode={() => view.mode, (m) => ((view.mode = m), (page = 1))} />
          </div>
        </div>

        {#if view.mode === 'grid'}
          <div>
            <div class="grid grid-cols-1 gap-3 sm:grid-cols-2 lg:grid-cols-4">
              {#each paginated as tag (tag.id)}
                <article
                  class="group relative flex min-h-28 flex-col rounded-xl border border-neutral-200 bg-white p-3 shadow-sm transition-all hover:-translate-y-px hover:border-neutral-300 hover:shadow-md dark:border-white/[0.08] dark:bg-white/[0.05] dark:hover:border-white/[0.14]"
                >
                  <a use:link href="/tags/{encodeURIComponent(tag.name)}" class="absolute inset-0 rounded-xl" aria-label="Open {tag.name}"></a>
                  <div class="flex items-start gap-3">
                    <div class="flex size-8 shrink-0 items-center justify-center rounded-lg border border-neutral-200 bg-neutral-50 text-neutral-500 dark:border-white/[0.08] dark:bg-white/[0.04] dark:text-fg-dim">
                      <Reicon name="tags" class="size-4" />
                    </div>
                    <div class="min-w-0 flex-1">
                      <h2 class="truncate text-[13px]! leading-4! font-semibold! text-black dark:text-fg">{tag.name}</h2>
                      <p class="mt-0.5 truncate text-[11px] text-neutral-500 dark:text-fg-faint">Team tag</p>
                    </div>
                  </div>
                  <div class="mt-auto flex items-center pt-4">
                    <span class="text-[11px] text-neutral-500 dark:text-fg-dim">{resources(tag)} {resources(tag) === 1 ? 'resource' : 'resources'}</span>
                  </div>
                </article>
              {/each}
            </div>
            {#if filtered.length > 0}
              <Pager
                total={filtered.length}
                bind:page
                bind:pageSize
                storageKey="coolify.page-size.tags"
                options={[12, 24, 48, 96]}
                class="mt-3 rounded-xl border border-neutral-200 bg-white shadow-sm dark:border-white/[0.08] dark:bg-white/[0.05]"
              />
            {/if}
          </div>
        {:else}
          <div class="overflow-hidden rounded-xl border border-neutral-200 bg-white shadow-sm dark:border-white/[0.08] dark:bg-white/[0.05]">
            <div class="tags-table-grid border-b border-neutral-200 bg-neutral-50 px-4 py-2.5 text-[11px] font-medium text-neutral-500 dark:border-white/[0.08] dark:bg-white/[0.05] dark:text-fg-faint">
              <div>Tag</div>
              <div>Resources</div>
              <div>Applications</div>
              <div class="tag-services">Services</div>
            </div>
            {#each paginated as tag (tag.id)}
              <a
                use:link
                href="/tags/{encodeURIComponent(tag.name)}"
                class="tags-table-grid group min-h-14 items-center border-b border-neutral-200 px-4 py-2.5 transition-colors last:border-b-0 hover:bg-neutral-50 hover:no-underline dark:border-white/[0.07] dark:hover:bg-white/[0.025]"
                aria-label="Open {tag.name}"
              >
                <div class="flex min-w-0 items-center gap-3">
                  <div class="flex size-8 shrink-0 items-center justify-center rounded-lg border border-neutral-200 bg-neutral-50 text-neutral-500 dark:border-white/[0.08] dark:bg-white/[0.035] dark:text-fg-dim">
                    <Reicon name="tags" class="size-4" />
                  </div>
                  <span class="truncate text-[13px] font-semibold text-black dark:text-fg">{tag.name}</span>
                </div>
                <div class="text-[12px] text-neutral-600 dark:text-fg-dim">{resources(tag)}</div>
                <div class="text-[12px] text-neutral-600 dark:text-fg-dim">{tag.applications_count}</div>
                <div class="tag-services text-[12px] text-neutral-600 dark:text-fg-dim">{tag.services_count}</div>
              </a>
            {/each}
            {#if filtered.length > 0}
              <Pager total={filtered.length} bind:page bind:pageSize storageKey="coolify.page-size.tags" options={[12, 24, 48, 96]} />
            {/if}
          </div>
        {/if}

        {#if filtered.length === 0}
          <div class="flex min-h-52 flex-col items-center justify-center rounded-xl border border-neutral-200 bg-white px-6 text-center dark:border-white/[0.08] dark:bg-white/[0.05]">
            <Reicon name="search" class="mb-3 size-6 text-neutral-300 dark:text-fg-faint" />
            <p class="text-[13px] font-medium">No matching tags</p>
            <p class="mt-1 text-[12px] text-neutral-500 dark:text-fg-dim">Try a different search.</p>
          </div>
        {/if}
      </div>
    {:else}
      <div class="flex flex-col gap-6">
        <div class="grid grid-cols-1 gap-3 sm:grid-cols-3">
          <div class="rounded-xl border border-neutral-200 bg-white p-3 dark:border-white/[0.08] dark:bg-white/[0.05]">
            <p class="text-[11px] font-medium text-neutral-500 dark:text-fg-faint">Resources</p>
            <p class="mt-1 text-[20px] font-semibold tracking-tight text-black dark:text-fg">{resourceCount}</p>
            <p class="mt-0.5 text-[11px] text-neutral-500 dark:text-fg-dim">Applications & services</p>
          </div>
          <div class="rounded-xl border border-neutral-200 bg-white p-3 dark:border-white/[0.08] dark:bg-white/[0.05]">
            <p class="text-[11px] font-medium text-neutral-500 dark:text-fg-faint">Applications</p>
            <p class="mt-1 text-[20px] font-semibold tracking-tight text-black dark:text-fg">{details.applications.length}</p>
            <p class="mt-0.5 text-[11px] text-neutral-500 dark:text-fg-dim">Using this tag</p>
          </div>
          <div class="rounded-xl border border-neutral-200 bg-white p-3 dark:border-white/[0.08] dark:bg-white/[0.05]">
            <p class="text-[11px] font-medium text-neutral-500 dark:text-fg-faint">Active deployments</p>
            <p class="mt-1 text-[20px] font-semibold tracking-tight text-black dark:text-fg">{details.deployments.length}</p>
            <p class="mt-0.5 text-[11px] text-neutral-500 dark:text-fg-dim">Queued or running</p>
          </div>
        </div>

        <SettingsSection title={details.tag.name} description="Use this webhook to deploy every resource with this tag.">
          {#snippet actions()}
            <button type="button" class="button whitespace-nowrap" onclick={openConnect}>
              <Reicon name="link" class="size-3.5" />
              Connect to resources
            </button>
            <a
              href="/canvas/tags?select=tag%3A{details!.tag.uuid}"
              class="button hidden whitespace-nowrap md:inline-flex"
              title="Add this tag to resources on the canvas, and see which resources have it"
            >
              <Reicon name="deploy-canvas" class="size-3.5" />
              Connect in canvas
            </a>
            {#if details!.can_delete}
              <button type="button" class="button button-error" onclick={() => (deleting = true)}>Delete</button>
            {/if}
            <button type="button" class="button" onclick={() => (redeploying = true)}>Redeploy all</button>
          {/snippet}
          <FormInput id="webhook" label="Deploy webhook URL" readonly value={details.webhook} />
        </SettingsSection>

        <SettingsSection title="Resources" description="{resourceCount} {resourceCount === 1 ? 'resource' : 'resources'} use this tag.">
          {#if resourceCount === 0}
            <Empty title="No resources use this tag" description="Connect this tag to resources to see them here." iconName="tags" size="sm" />
          {:else}
            <div class="grid grid-cols-1 gap-3 sm:grid-cols-2 lg:grid-cols-4">
              {#each details.applications as a (a.uuid)}
                <a
                  use:link
                  href="/project/{a.project_uuid}/environment/{a.environment_uuid}/application/{a.uuid}"
                  class="group flex min-h-24 flex-col rounded-lg border border-neutral-200 bg-neutral-50/70 p-3 transition-colors hover:border-neutral-300 hover:no-underline dark:border-white/[0.08] dark:bg-white/[0.05] dark:hover:border-white/[0.14]"
                >
                  <div class="flex items-start gap-2.5">
                    <div class="flex size-7 shrink-0 items-center justify-center overflow-hidden rounded-md border border-neutral-200 bg-white text-neutral-500 dark:border-white/[0.08] dark:bg-white/[0.035] dark:text-fg-dim">
                      <ResourceLogo
                        application={{ buildPack: a.build_pack ?? null, sourceType: a.source_type ?? null, fqdn: a.fqdn ?? null, basicAuth: !!a.is_http_basic_auth_enabled }}
                        iconClass="size-3.5"
                      />
                    </div>
                    <div class="min-w-0">
                      <h3 class="truncate text-[12px]! leading-4! font-semibold! text-black dark:text-fg">{a.name}</h3>
                      <p class="mt-0.5 truncate text-[11px] text-neutral-500 dark:text-fg-faint">{a.project}/{a.environment}</p>
                    </div>
                  </div>
                  <p class="mt-auto truncate pt-3 text-[11px] text-neutral-500 dark:text-fg-dim">{a.description || 'Application'}</p>
                </a>
              {/each}
              {#each details.services as s (s.uuid)}
                <a
                  use:link
                  href="/project/{s.project_uuid}/environment/{s.environment_uuid}/service/{s.uuid}"
                  class="group flex min-h-24 flex-col rounded-lg border border-neutral-200 bg-neutral-50/70 p-3 transition-colors hover:border-neutral-300 hover:no-underline dark:border-white/[0.08] dark:bg-white/[0.05] dark:hover:border-white/[0.14]"
                >
                  <div class="flex items-start gap-2.5">
                    <div class="flex size-7 shrink-0 items-center justify-center overflow-hidden rounded-md border border-neutral-200 bg-white text-neutral-500 dark:border-white/[0.08] dark:bg-white/[0.035] dark:text-fg-dim">
                      <ResourceLogo iconClass="size-3.5" />
                    </div>
                    <div class="min-w-0">
                      <h3 class="truncate text-[12px]! leading-4! font-semibold! text-black dark:text-fg">{s.name}</h3>
                      <p class="mt-0.5 truncate text-[11px] text-neutral-500 dark:text-fg-faint">{s.project}/{s.environment}</p>
                    </div>
                  </div>
                  <p class="mt-auto truncate pt-3 text-[11px] text-neutral-500 dark:text-fg-dim">{s.description || 'Service'}</p>
                </a>
              {/each}
            </div>
          {/if}
        </SettingsSection>

        <SettingsSection title="Active deployments" description="Queued and running deployments for applications using this tag." flush>
          <div class="overflow-x-auto">
            {#if details.deployments.length === 0}
              <div class="p-3">
                <Empty title="No active deployments" description="Deployments will appear here while they are queued or running." iconName="play-circle" size="sm" />
              </div>
            {:else}
              <div class="grid min-w-[620px] grid-cols-[minmax(0,1fr)_minmax(10rem,.55fr)_7rem] border-b border-neutral-200 bg-neutral-50 px-4 py-2.5 text-[11px] font-medium text-neutral-500 dark:border-white/[0.08] dark:bg-white/[0.05] dark:text-fg-faint">
                <div>Resource</div>
                <div>Server</div>
                <div>Status</div>
              </div>
              {#each details.deployments as d (d.id)}
                <a
                  href={d.deployment_url ?? undefined}
                  class="grid min-h-13 min-w-[620px] grid-cols-[minmax(0,1fr)_minmax(10rem,.55fr)_7rem] items-center border-b border-neutral-200 px-4 py-2.5 text-[12px] transition-colors last:border-b-0 hover:bg-neutral-50 hover:no-underline dark:border-white/[0.07] dark:hover:bg-white/[0.025]"
                >
                  <div class="truncate font-medium text-black dark:text-fg">{d.application_name}</div>
                  <div class="truncate text-neutral-500 dark:text-fg-dim">{d.server_name}</div>
                  <div><StatusBadge label={headline(d.status)} type={d.status === 'in_progress' ? 'warning' : 'neutral'} /></div>
                </a>
              {/each}
            {/if}
          </div>
        </SettingsSection>
      </div>
    {/if}
  {/if}
</div>

{#if creating}
  <Modal title="New tag" onclose={() => (creating = false)}>
    <form onsubmit={createTag} class="flex w-full flex-col gap-4">
      <FormInput
        id="newTagName"
        label="Name"
        placeholder="production"
        required
        helper="Lowercase, without spaces. You can add resources to the tag later."
        bind:value={newName}
      />
      <div class="flex justify-end border-t border-neutral-200 pt-4 dark:border-white/[0.08]">
        <button type="submit" class="button button-highlighted">Create tag</button>
      </div>
    </form>
  </Modal>
{/if}

{#if connectOpen}
  <Modal title="Connect to resources" subtitle="Add this tag to resources, or remove it." onclose={() => (connectOpen = false)}>
    {#if connectable}
      {#each groups as [context, list] (context)}
        <div class="mb-4 last:mb-0">
          <p class="mb-1.5 truncate text-[11px] font-medium text-neutral-500 dark:text-fg-faint">{context}</p>
          <div class="flex flex-col gap-1">
            {#each list as r (r.uuid)}
              <label class="flex items-center gap-2.5 rounded-md border border-neutral-200 px-3 py-2 text-[13px] dark:border-white/[0.08] {r.can_update ? 'cursor-pointer' : 'opacity-60'}">
                <input
                  type="checkbox"
                  checked={r.tagged}
                  disabled={!r.can_update}
                  onclick={() => toggleResource(r)}
                  class="size-4 shrink-0 rounded-sm border-neutral-300 text-accent focus:ring-accent dark:border-white/20 dark:bg-transparent"
                />
                <span class="min-w-0 truncate">{r.name}</span>
              </label>
            {/each}
          </div>
        </div>
      {:else}
        <Empty title="No resources yet" description="Create a resource in a project first." iconName="tags" size="sm" />
      {/each}
    {/if}
  </Modal>
{/if}

{#if deleting && details}
  <ConfirmModal
    title="Delete this tag?"
    actions={['The tag will be removed from every resource.', 'The deploy webhook of this tag will stop working.']}
    confirmationText={details.tag.name}
    confirmationLabel="Please confirm the execution of the actions by entering the Tag Name below"
    shortConfirmationLabel="Tag Name"
    step2ButtonText="Delete tag"
    onconfirm={deleteTag}
    onclose={() => (deleting = false)}
  />
{/if}

{#if redeploying && details}
  <ConfirmModal
    title="Redeploy all resources with this tag?"
    actions={['All resources with this tag will be redeployed.', 'During redeploy resources will be temporarily unavailable.']}
    confirmationText={details.tag.name}
    confirmationLabel="Please confirm the execution of the actions by entering the Tag Name below"
    shortConfirmationLabel="Tag Name"
    step2ButtonText="Redeploy All"
    onconfirm={redeployAll}
    onclose={() => (redeploying = false)}
  />
{/if}
