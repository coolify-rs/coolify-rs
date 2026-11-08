<script lang="ts">
  // Port of upstream livewire/shared-variables/team/index.blade.php,
  // livewire/shared-variables/project/show.blade.php,
  // livewire/shared-variables/environment/show.blade.php and
  // livewire/shared-variables/server/show.blade.php with
  // components/shared-variables/editor.blade.php and
  // livewire/project/shared/environment-variable/add.blade.php.
  import Reicon from '../components/Reicon.svelte';
  import Empty from '../components/Empty.svelte';
  import Modal from '../components/Modal.svelte';
  import Dropdown from '../components/Dropdown.svelte';
  import FormInput from '../components/FormInput.svelte';
  import Callout from '../components/Callout.svelte';
  import FormTextarea from '../components/FormTextarea.svelte';
  import Listbox from '../components/Listbox.svelte';
  import SettingsSection from '../components/SettingsSection.svelte';
  import UnsavedBar from '../components/UnsavedBar.svelte';
  import SharedVariablesLayout from '../components/SharedVariablesLayout.svelte';
  import SharedVariableRow from '../components/SharedVariableRow.svelte';
  import { ApiError, uiApi } from '../lib/api';
  import { setProjectCrumbs } from '../lib/crumbs';
  import { sharedVariablesUi, type ScopeData, type SharedVariable } from '../lib/sharedVariablesApi';
  import { router } from '../lib/router.svelte';
  import { notify } from '../lib/toast.svelte';
  import { page } from '../lib/page.svelte';

  let { kind, uuid, env }: { kind: string; uuid?: string; env?: string } = $props();

  // svelte-ignore state_referenced_locally
  const scope = { kind, uuid, env };
  const titles: Record<string, string> = { team: 'Team', project: 'Project', environment: 'Environment', server: 'Server' };
  // svelte-ignore state_referenced_locally
  document.title = `${titles[kind]} Variables | coolify-rs`;
  // svelte-ignore state_referenced_locally
  const scopeLabel = titles[kind];
  // svelte-ignore state_referenced_locally
  const variablesLabel = `${titles[kind]} shared variables`;
  const canvasUrl = $derived(
    kind === 'team'
      ? '/shared-variables/team/canvas'
      : kind === 'project'
        ? `/shared-variables/project/${uuid}/canvas`
        : kind === 'environment'
          ? `/shared-variables/environments/project/${uuid}/environment/${env}/canvas`
          : `/shared-variables/server/${uuid}/canvas`,
  );

  let data = $state<ScopeData | null>(null);
  let view = $state<'normal' | 'dev'>('normal');
  let search = $state('');
  let sort = $state<'alphabetical' | 'creation'>('alphabetical');
  let devText = $state('');
  let savedDev = $state('');
  let saving = $state(false);

  async function load() {
    const d = await sharedVariablesUi.scope(scope);
    if (!d.found) {
      router.go('/', true);
      return;
    }
    data = d;
    devText = d.dev_view;
    savedDev = d.dev_view;
  }
  load();

  if (kind === 'project' || kind === 'environment') {
    uiApi.project(uuid!).then((p) => {
      setProjectCrumbs(p, env);
      if (kind === 'project') {
        page.switchers = page.switchers.map((s) => ({
          ...s,
          items: s.items.map((i) => ({ ...i, href: i.href.replace('/project/', '/shared-variables/project/') })),
        }));
      }
    });
  }

  const alphabetical = $derived(
    [...(data?.variables ?? [])].sort((a, b) => (a.key < b.key ? -1 : a.key > b.key ? 1 : 0)).map((v) => v.id),
  );
  const rowText = (v: SharedVariable) => `${v.key} ${v.comment ?? ''} ${scopeLabel}`.toLowerCase();
  const visible = $derived((data?.variables ?? []).filter((v) => rowText(v).includes(search.trim().toLowerCase())));
  const filteredCount = $derived(visible.length);
  const position = (v: SharedVariable, index: number) => (sort === 'alphabetical' ? alphabetical.indexOf(v.id) : index);
  const canvasLink = (v: SharedVariable) => `${canvasUrl}?select=variable%3A${v.id}`;

  async function saveBulk() {
    saving = true;
    try {
      const r = await sharedVariablesUi.bulk(scope, devText);
      if (r.message) notify.success(r.message);
    } catch (e) {
      const message = e instanceof ApiError ? e.message : String(e);
      notify.failure(message);
      // `submit()` returns before `refreshEnvs()` when a "# coolify:" line does not parse
      // (`parseEnvDevView` names the line): the edited text stays.
      if (/^Line \d+: /.test(message)) {
        saving = false;
        return;
      }
    } finally {
      saving = false;
    }
    await load();
  }

  // Add variable.
  let adding = $state(false);
  let newKey = $state('');
  let newValue = $state('');
  let newComment = $state('');
  let newMultiline = $state(false);
  let addError = $state<ApiError | null>(null);

  async function addVariable(e: SubmitEvent) {
    e.preventDefault();
    adding = false;
    addError = null;
    try {
      await sharedVariablesUi.add(scope, { key: newKey, value: newValue, comment: newComment === '' ? null : newComment, is_multiline: newMultiline });
      newKey = '';
      newValue = '';
      newComment = '';
      newMultiline = false;
      await load();
    } catch (err) {
      const failure = err instanceof ApiError ? err : new ApiError(0, String(err));
      if (Object.keys(failure.errors).length) addError = failure;
      else notify.failure(failure.message);
    }
  }
</script>

<div>
  <SharedVariablesLayout>
    {#if data}
      <div class="application-settings-form w-full">
        <SettingsSection title={data.title} flush>
          {#snippet actions()}
            <button type="button" class="button whitespace-nowrap" onclick={() => (view = view === 'normal' ? 'dev' : 'normal')}>
              <Reicon name={view === 'normal' ? 'browser-code' : 'unordered-list'} class="size-3.5" />
              <span class="max-sm:hidden">{view === 'normal' ? 'Developer view' : 'Normal view'}</span>
              <span class="sm:hidden">{view === 'normal' ? 'Developer' : 'Normal'}</span>
            </button>
            <div class="view-toggle">
              <a
                href={canvasUrl}
                class="relative flex h-7.5 shrink-0 items-center gap-1.5 whitespace-nowrap rounded-md px-2.5 text-[12px] font-medium transition-colors text-neutral-400 hover:bg-neutral-100 hover:text-black dark:hover:text-white"
                title="Canvas view (beta): see everything and how it connects"
              >
                <Reicon name="deploy-canvas" class="size-3.5" />
                Canvas
                <span class="ml-0.5 inline-flex items-center rounded-full bg-coollabs px-1.5 py-px text-[10px] leading-4 font-semibold text-white"> Try it </span>
              </a>
            </div>
          {/snippet}

          {#if view === 'normal'}
            <div class="flex flex-col gap-3 border-b border-neutral-200 p-3 sm:flex-row sm:items-center sm:justify-between dark:border-white/[0.08]">
              <div class="relative w-full sm:max-w-sm">
                <Reicon name="search" class="pointer-events-none absolute top-1/2 left-2.5 z-10 size-3.5 -translate-y-1/2 text-neutral-400 dark:text-fg-faint" />
                <input
                  bind:value={search}
                  type="search"
                  placeholder="Search variables"
                  class="h-8! w-full rounded-lg! border-neutral-200! bg-white! py-0! pr-8! pl-8! text-[12px]! shadow-none! placeholder:text-neutral-400 focus:border-accent! focus:ring-0! dark:border-white/[0.08]! dark:bg-white/[0.035]! dark:text-fg! dark:placeholder:text-fg-faint"
                />
                {#if search}
                  <button
                    onclick={() => (search = '')}
                    type="button"
                    class="absolute top-1/2 right-2 flex size-5 -translate-y-1/2 items-center justify-center rounded text-neutral-400 transition-colors hover:bg-neutral-100 hover:text-black dark:text-fg-faint dark:hover:bg-white/[0.07] dark:hover:text-fg"
                    aria-label="Clear search"
                  >
                    <Reicon name="x" class="size-3" />
                  </button>
                {/if}
              </div>

              <div class="flex items-center gap-2">
                <Dropdown panelClass="w-48!">
                  {#snippet trigger({ open })}
                    <button type="button" class="button" aria-haspopup="listbox" aria-expanded={open}>
                      <Reicon name="sort-direction" class="size-3.5" />
                      Sort
                    </button>
                  {/snippet}
                  {#snippet children({ close })}
                    {#each [['alphabetical', 'Alphabetical'], ['creation', 'Creation order']] as [value, label] (value)}
                      <button
                        type="button"
                        class="listbox-option"
                        onclick={() => {
                          sort = value as 'alphabetical' | 'creation';
                          close();
                        }}
                      >
                        <span>{label}</span>
                        {#if sort === value}
                          <svg class="size-3.5 shrink-0" viewBox="0 0 24 24" fill="none">
                            <path d="M5 12l5 5 9-11" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" />
                          </svg>
                        {/if}
                      </button>
                    {/each}
                  {/snippet}
                </Dropdown>

                {#if data.can_update}
                  <button type="button" class="button button-highlighted" onclick={() => (adding = true)}>
                    <Reicon name="plus" class="size-3.5" />
                    Add variable
                  </button>
                {/if}
              </div>
            </div>

            {#if data.variables.length === 0}
              <div class="p-3">
                <Empty title="No shared variables" description="Add a variable to make it available to resources in this scope." iconName="variables" size="sm" />
              </div>
            {:else}
              <div class="data-table flex w-full flex-col">
                <div class="data-table-header env-table-grid-shared order-[-1]">
                  <span>Name</span>
                  <span>Scope</span>
                  <span>{kind === 'server' ? 'Value / comment' : 'Comment'}</span>
                  <span class="text-center">Multiline</span>
                  <span class="text-right">Actions</span>
                </div>
                {#each data.variables as v, i (v.id)}
                  {#if visible.includes(v)}
                    {#if v.read_only}
                      <div class="env-table-item" style="order: {position(v, i)}">
                        <div class="data-table-row env-table-grid-shared">
                          <div class="min-w-0">
                            <div class="env-key-label truncate font-mono text-[13px]" title={v.key}>{v.key}</div>
                            <div class="text-[11px] text-neutral-500 dark:text-fg-dim">Built-in · Read-only</div>
                          </div>
                          <span class="env-type-desktop text-[13px] text-neutral-500 dark:text-fg-dim">{scopeLabel}</span>
                          <span class="min-w-0 truncate font-mono text-[13px]" title={v.value ?? undefined}>{v.value}</span>
                          <span class="data-table-cell-dash">-</span>
                          <span class="justify-self-end text-neutral-400 dark:text-fg-faint" title="Built-in variable, managed by coolify-rs"><Reicon name="keys" class="size-3.5" /></span>
                        </div>
                      </div>
                    {:else}
                      <SharedVariableRow variable={v} {scopeLabel} canUpdate={data.can_update} connectLink={canvasLink(v)} order={position(v, i)} onchange={load} />
                    {/if}
                  {/if}
                {/each}
                <div class="order-[9999] flex min-h-11 items-center border-t border-neutral-200 px-4 text-[11px] text-neutral-500 dark:border-white/[0.08] dark:text-fg-faint">
                  <span>{filteredCount} {filteredCount === 1 ? 'variable' : 'variables'}</span>
                </div>
              </div>
            {/if}
          {:else}
            {#if data.variables.some((v) => v.read_only)}
              <div class="border-b border-neutral-200 p-4 dark:border-white/[0.08]">
                <div class="mb-2 text-[12px] text-neutral-500 dark:text-fg-dim">Built-in · Read-only</div>
                {#each [...data.variables.filter((v) => v.read_only)].sort((a, b) => (a.key < b.key ? -1 : 1)) as v (v.id)}
                  <div class="break-all font-mono text-[13px]">{v.key}={v.value}</div>
                {/each}
              </div>
            {/if}
            <form
              class="flex flex-col gap-4 p-4"
              onsubmit={(e) => {
                e.preventDefault();
                saveBulk();
              }}
            >
              <UnsavedBar dirty={devText !== savedDev} {saving} onsave={saveBulk} onreset={() => (devText = savedDev)} />
              <Callout type="info" title="Note">
                To make a variable literal, put <code class="font-mono"># coolify: literal</code> directly above
                its line. Without this line, the flag does not change.
              </Callout>
              <FormTextarea rows={20} id="variables" monospace label={variablesLabel} disabled={!data.can_update} bind:value={devText} />
            </form>
          {/if}
        </SettingsSection>
      </div>
    {/if}
  </SharedVariablesLayout>
</div>

{#if adding}
  <Modal title="New Shared Variable" onclose={() => (adding = false)}>
    <form class="flex w-full flex-col gap-4" onsubmit={addVariable}>
      <div class="grid gap-4 sm:grid-cols-2">
        <FormInput placeholder="NODE_ENV" id="key" label="Name" required bind:value={newKey} error={addError?.field('key')} />
        <FormInput
          id="comment"
          label="Comment"
          placeholder="Optional note"
          helper="Add a note to document what this environment variable is used for."
          maxlength={256}
          bind:value={newComment}
          error={addError?.field('comment')}
        />
      </div>
      <div>
        {#if newMultiline}
          <FormTextarea id="value" label="Value" required bind:value={newValue} />
        {:else}
          <FormInput placeholder="production" id="value" label="Value" required bind:value={newValue} />
        {/if}
      </div>
      <div class="grid gap-4 border-t border-neutral-200 pt-4 dark:border-white/[0.07] sm:grid-cols-2">
        <Listbox
          id="is_multiline"
          label="Value type"
          bind:value={newMultiline}
          options={[
            { value: false, label: 'Single line' },
            { value: true, label: 'Multiline' },
          ]}
        />
      </div>
      <div class="flex justify-end border-t border-neutral-200 pt-4 dark:border-white/[0.07]">
        <button type="submit" class="button">Add variable</button>
      </div>
    </form>
  </Modal>
{/if}
