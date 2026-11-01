<script lang="ts">
  // Port of upstream livewire/project/shared/environment-variable/all.blade.php and add.blade.php
  // (Project\Shared\EnvironmentVariable\All, Add).
  import Reicon from '../Reicon.svelte';
  import Modal from '../Modal.svelte';
  import FormInput from '../FormInput.svelte';
  import FormTextarea from '../FormTextarea.svelte';
  import Listbox from '../Listbox.svelte';
  import Checkbox from '../Checkbox.svelte';
  import Callout from '../Callout.svelte';
  import Dropdown from '../Dropdown.svelte';
  import Empty from '../Empty.svelte';
  import Pager from '../Pager.svelte';
  import SettingsSection from '../SettingsSection.svelte';
  import SkeletonTable from '../SkeletonTable.svelte';
  import UnsavedBar from '../UnsavedBar.svelte';
  import EnvRow from './EnvRow.svelte';
  import EnvHardcodedRow from './EnvHardcodedRow.svelte';
  import EnvVarInput from './EnvVarInput.svelte';
  import { ApiError } from '../../lib/api';
  import { envsUi, type AvailableVars, type EnvList, type ResourceKind } from '../../lib/sharedApi';
  import { buildWarning } from '../../lib/envWarnings';
  import { notify } from '../../lib/toast.svelte';

  let {
    kind,
    uuid,
    /** Compose variables changed by another section re-read the list. */
    onconfigurationchanged,
  }: { kind: ResourceKind; uuid: string; onconfigurationchanged?: () => void } = $props();

  const api = $derived(envsUi(kind, uuid));
  const filterOptions = [
    ['managed', 'Managed'],
    ['user', 'User-defined'],
    ['buildtime', 'Buildtime'],
    ['runtime', 'Runtime'],
    ['multiline', 'Multiline'],
    ['literal', 'Literal'],
  ] as const;
  const filterLabels: Record<string, string> = Object.fromEntries(filterOptions);
  const environmentOptions = [
    ['all', 'All environments'],
    ['production', 'Production'],
    ['preview', 'Preview'],
  ] as const;
  const sortOptions = [
    ['default', 'Default order'],
    ['name_asc', 'Name A–Z'],
    ['name_desc', 'Name Z–A'],
  ] as const;

  let data = $state<EnvList | null>(null);
  let view = $state<'normal' | 'dev'>('normal');
  let search = $state('');
  let variableFilters = $state<string[]>([]);
  let serviceFilters = $state<string[]>([]);
  let environmentFilter = $state('all');
  let tableSort = $state('default');
  let page = $state(1);
  let perPage = $state(10);
  let available = $state<AvailableVars>({ team: [], project: [], environment: [], server: [] });
  let focusKey = $state<string | null>(null);

  let sorting = $state(false);
  let buildSecrets = $state(false);
  let defaults = $state(true);
  let redactAll = $state(true);

  let variables = $state('');
  let variablesPreview = $state('');
  let savedVariables = $state('');
  let savedVariablesPreview = $state('');
  let saving = $state(false);
  const dirty = $derived(variables !== savedVariables || variablesPreview !== savedVariablesPreview);

  let adding = $state(false);
  let addKey = $state('');
  let addComment = $state('');
  let addValue = $state('');
  let addMultiline = $state(false);
  let addLiteral = $state(false);
  let addBuildtime = $state(true);
  let addRuntime = $state(true);
  let addError = $state<ApiError | null>(null);
  const addWarning = $derived(buildWarning(addKey, addValue, addBuildtime));

  const canManage = $derived(data?.can_manage ?? false);
  const showPreview = $derived(data?.show_preview ?? false);
  const activeFilterCount = $derived(variableFilters.length + serviceFilters.length + (environmentFilter !== 'all' ? 1 : 0));
  const activeFilterText = $derived(
    [
      ...variableFilters.map((f) => filterLabels[f] ?? f),
      ...serviceFilters,
      ...(environmentFilter !== 'all' ? [environmentFilter[0].toUpperCase() + environmentFilter.slice(1)] : []),
    ].join(', '),
  );
  const rows = $derived(data?.rows ?? []);
  const total = $derived(data?.total ?? 0);

  async function load() {
    const d = await api.list({ search: search.trim(), filters: variableFilters, services: serviceFilters, env: environmentFilter, sort: tableSort, page, per_page: perPage });
    data = d;
    page = d.page;
    sorting = d.is_env_sorting_enabled;
    buildSecrets = d.use_build_secrets;
    defaults = d.include_coolify_default_variables;
    redactAll = d.redact_all_env_values_in_logs;
  }

  async function loadDev() {
    const d = await api.dev();
    variables = savedVariables = d.variables;
    variablesPreview = savedVariablesPreview = d.variables_preview ?? '';
  }

  $effect(() => {
    load();
    api.available().then((a) => (available = a)).catch(() => {});
  });

  function refresh() {
    onconfigurationchanged?.();
    return load();
  }

  let searchTimer: ReturnType<typeof setTimeout> | undefined;
  function onsearch() {
    clearTimeout(searchTimer);
    searchTimer = setTimeout(() => {
      page = 1;
      load();
    }, 300);
  }

  function toggleVariableFilter(f: string) {
    if (variableFilters.includes(f)) variableFilters = variableFilters.filter((x) => x !== f);
    else {
      let next = variableFilters;
      if (f === 'managed') next = next.filter((x) => x !== 'user');
      if (f === 'user') next = next.filter((x) => x !== 'managed');
      variableFilters = [...next, f];
    }
    page = 1;
    load();
  }

  function toggleServiceFilter(s: string) {
    serviceFilters = serviceFilters.includes(s) ? serviceFilters.filter((x) => x !== s) : [...serviceFilters, s];
    page = 1;
    load();
  }

  function setEnvironmentFilter(v: string) {
    environmentFilter = v;
    page = 1;
    load();
  }

  function setTableSort(v: string) {
    tableSort = v;
    page = 1;
    load();
  }

  function clearFilters() {
    variableFilters = [];
    serviceFilters = [];
    environmentFilter = 'all';
    page = 1;
    load();
  }

  async function switchView() {
    view = view === 'normal' ? 'dev' : 'normal';
    if (view === 'dev') await loadDev();
    else await load();
  }

  async function saveSetting(values: Record<string, boolean>) {
    try {
      const r = await api.settings(values);
      notify.success(r.message);
      onconfigurationchanged?.();
      page = 1;
      await load();
    } catch (err) {
      notify.failure(err instanceof ApiError ? err.message : String(err));
      await load();
    }
  }

  async function submitBulk() {
    if (saving) return;
    saving = true;
    try {
      const r = await api.bulk(variables, showPreview ? variablesPreview : null);
      if (r.error) notify.failure(r.error);
      if (r.message) notify.success(r.message);
      await loadDev();
      onconfigurationchanged?.();
    } catch (err) {
      notify.failure(err instanceof ApiError ? err.message : String(err));
    } finally {
      saving = false;
    }
  }

  function openAdd() {
    addKey = '';
    addComment = '';
    addValue = '';
    addMultiline = false;
    addLiteral = false;
    addBuildtime = true;
    addRuntime = true;
    addError = null;
    adding = true;
  }

  async function submitAdd(e: Event) {
    e.preventDefault();
    addError = null;
    try {
      const r = await api.add({
        key: addKey,
        value: addValue,
        comment: addComment === '' ? null : addComment,
        is_multiline: addMultiline,
        is_literal: addLiteral,
        is_runtime: addRuntime,
        is_buildtime: addBuildtime,
      });
      notify.success(r.message);
      adding = false;
      await refresh();
    } catch (err) {
      const f = err instanceof ApiError ? err : new ApiError(0, String(err));
      if (Object.keys(f.errors).length) addError = f;
      else {
        notify.failure(f.message);
        adding = false;
      }
    }
  }

  async function focusCompose(key: string) {
    if (!rows.some((r) => r.kind === 'managed' && r.key === key)) {
      // `focusComposeEnvironmentVariable`: clear what hides the row and find it.
      search = key;
      variableFilters = [];
      serviceFilters = [];
      environmentFilter = 'all';
      page = 1;
      await load();
    }
    focusKey = null;
    queueMicrotask(() => (focusKey = key));
  }
</script>

<div class="flex flex-col gap-4">
  <SettingsSection id="environment-variables-section" title="Environment variables" description="Environment variables (secrets) for this resource.">
    {#snippet actions()}
      {#if canManage}
        <button type="button" class="button" onclick={switchView}>{view === 'normal' ? 'Developer view' : 'Normal view'}</button>
      {/if}
    {/snippet}
    {#if view === 'normal'}
      <div class="flex w-full flex-col gap-4">
        {#if data?.is_application}
          <div class="grid w-full items-end gap-4 sm:grid-cols-2">
            {#if data.build_pack !== 'dockercompose'}
              <Listbox
                id="is_env_sorting_enabled"
                label="Environment variable order"
                helper="Controls how environment variables are ordered in the list and written to the generated .env file on deploy."
                disabled={!canManage}
                bind:value={sorting}
                onchange={() => saveSetting({ is_env_sorting_enabled: sorting, use_build_secrets: buildSecrets })}
                options={[{ value: false, label: 'Creation order' }, { value: true, label: 'Alphabetical' }]}
              />
            {/if}
            <Listbox
              id="use_build_secrets"
              label="Build secrets"
              helper="Docker BuildKit secrets keep values out of the final image for enhanced security during builds. Requires Docker 18.09+ with BuildKit support."
              disabled={!canManage}
              bind:value={buildSecrets}
              onchange={() => saveSetting({ is_env_sorting_enabled: sorting, use_build_secrets: buildSecrets })}
              options={[{ value: false, label: 'Standard build arguments' }, { value: true, label: 'Docker BuildKit secrets' }]}
            />
          </div>
        {/if}
        <Checkbox
          id="include_coolify_default_variables"
          label="Include coolify-rs default variables"
          helper="Adds COOLIFY_APP_NAME, COOLIFY_PROJECT_NAME, COOLIFY_ENVIRONMENT_NAME and COOLIFY_SERVER_IP to the container at runtime. A variable that you define with the same name wins. Takes effect after the next deploy or restart."
          disabled={!canManage}
          bind:checked={defaults}
          onchange={() => saveSetting({ include_coolify_default_variables: defaults })}
        />
        {#if data?.has_log_redaction}
          <div class="grid w-full items-end gap-4 sm:grid-cols-2">
            <Listbox
              id="redact_all_env_values_in_logs"
              label="Environment values in logs"
              helper="Hide every environment variable value in deployment logs, scheduled task output, copied or downloaded logs, and logs from the API and MCP. Live log streaming is not changed. Locked variables and known secret formats are always hidden."
              disabled={!canManage}
              bind:value={redactAll}
              onchange={() => saveSetting({ redact_all_env_values_in_logs: redactAll })}
              options={[{ value: true, label: 'Hide all values' }, { value: false, label: 'Hide locked values only' }]}
            />
          </div>
        {/if}
      </div>
    {:else}
      <form
        onsubmit={(e) => {
          e.preventDefault();
          submitBulk();
        }}
        class="flex w-full flex-col gap-4"
      >
        {#if canManage}
          <Callout type="info" title="Note">
            Inline comments with space before # (e.g., <code class="font-mono">KEY=value #comment</code>) are stripped.
          </Callout>
          <FormTextarea rows={10} class="whitespace-pre-wrap font-sans" id="variables" label="Production" bind:value={variables} />
          {#if showPreview}
            <FormTextarea rows={10} class="whitespace-pre-wrap font-sans" id="variablesPreview" label="Preview deployments" bind:value={variablesPreview} />
          {/if}
          <UnsavedBar
            {dirty}
            {saving}
            onsave={submitBulk}
            onreset={() => {
              variables = savedVariables;
              variablesPreview = savedVariablesPreview;
            }}
          />
        {:else}
          <FormTextarea rows={10} class="whitespace-pre-wrap font-sans" id="variables" label="Production" disabled bind:value={variables} />
          {#if showPreview}
            <FormTextarea rows={10} class="whitespace-pre-wrap font-sans" id="variablesPreview" label="Preview deployments" disabled bind:value={variablesPreview} />
          {/if}
        {/if}
      </form>
    {/if}
  </SettingsSection>

  {#if view === 'normal'}
    <div class="table-toolbar flex flex-col gap-2 sm:flex-row sm:flex-wrap sm:items-center mt-2" aria-busy={!data}>
      <div class="min-w-0 w-full flex-1 sm:max-w-md">
        <div class="table-search relative min-w-0 w-full">
          <input type="search" placeholder="Search environment variables" aria-label="Search environment variables" class="input w-full pl-8!" bind:value={search} oninput={onsearch} disabled={!data} />
          <div class="pointer-events-none absolute inset-y-0 left-0 flex items-center pl-2.5">
            <Reicon name="search" class="size-3.5 text-neutral-400 dark:text-fg-faint" />
          </div>
        </div>
      </div>
      <div class="flex flex-wrap items-center gap-2 sm:ml-auto">
        <div class="table-filter">
          <Dropdown panelClass="w-44! overflow-hidden! p-0!">
            {#snippet trigger({ open })}
              <button type="button" aria-haspopup="listbox" aria-expanded={open} title={activeFilterText || undefined} class="button max-w-80 min-w-0 {activeFilterCount > 0 ? 'button-highlighted' : ''}">
                <Reicon name="filter" class="size-3.5 shrink-0" />
                <span class="truncate">{activeFilterText || 'Filter'}</span>
                {#if activeFilterCount > 0}
                  <span class="shrink-0 rounded-full bg-neutral-100 px-1.5 py-0.5 text-[10px] font-medium text-neutral-500 dark:bg-white/[0.07] dark:text-fg-dim">{activeFilterCount}</span>
                {/if}
              </button>
            {/snippet}
            {#snippet children({ close })}
              {#snippet mark(selected: boolean)}
                <span class="flex size-4 shrink-0 items-center justify-center rounded-[5px] border {selected ? 'border-coollabs bg-coollabs text-white dark:border-brand dark:bg-brand dark:text-white' : 'border-neutral-300 bg-white dark:border-white/[0.14] dark:bg-white/[0.045]'}">
                  {#if selected}
                    <svg class="size-3" viewBox="0 0 12 12" fill="none" aria-hidden="true">
                      <path d="m2.25 6.15 2.35 2.3 5.15-5" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" />
                    </svg>
                  {/if}
                </span>
              {/snippet}
              <div class="max-h-80 overflow-y-auto p-1">
                {#each filterOptions as [value, label] (value)}
                  <button type="button" class="listbox-option" onclick={() => toggleVariableFilter(value)}>
                    <span>{label}</span>
                    {@render mark(variableFilters.includes(value))}
                  </button>
                {/each}
                {#if showPreview}
                  <div class="my-1 border-t border-neutral-200 dark:border-white/10"></div>
                  {#each environmentOptions as [value, label] (value)}
                    <button
                      type="button"
                      class="listbox-option"
                      onclick={() => {
                        close();
                        setEnvironmentFilter(value);
                      }}
                    >
                      <span>{label}</span>
                      {@render mark(environmentFilter === value)}
                    </button>
                  {/each}
                {/if}
                {#if (data?.service_options.length ?? 0) > 0}
                  <div class="my-1 border-t border-neutral-200 dark:border-white/10"></div>
                  <div class="px-3 py-1 text-[11px] font-medium uppercase tracking-wide text-neutral-400 dark:text-fg-faint">Services</div>
                  {#each data?.service_options ?? [] as service (service)}
                    <button type="button" class="listbox-option" onclick={() => toggleServiceFilter(service)}>
                      <span class="truncate">{service}</span>
                      {@render mark(serviceFilters.includes(service))}
                    </button>
                  {/each}
                {/if}
              </div>
              <div class="border-t border-neutral-200 bg-white p-1 dark:border-white/10 dark:bg-raised">
                <button
                  type="button"
                  class="listbox-option text-neutral-500 dark:text-fg-dim"
                  disabled={activeFilterCount === 0}
                  onclick={() => {
                    close();
                    clearFilters();
                  }}
                >
                  <span>Reset filters</span>
                  <Reicon name="x" class="size-3.5" />
                </button>
              </div>
            {/snippet}
          </Dropdown>
        </div>
        <div class="table-sort">
          <Dropdown panelClass="w-44!">
            {#snippet trigger({ open })}
              <button type="button" class="button" aria-haspopup="listbox" aria-expanded={open}>
                <Reicon name="sort-direction" class="size-3.5" />
                Sort
              </button>
            {/snippet}
            {#snippet children({ close })}
              {#each sortOptions as [value, label] (value)}
                <button
                  type="button"
                  class="listbox-option"
                  onclick={() => {
                    close();
                    setTableSort(value);
                  }}
                >
                  <span>{label}</span>
                  {#if tableSort === value}<span>✓</span>{/if}
                </button>
              {/each}
            {/snippet}
          </Dropdown>
        </div>
        {#if canManage}
          <button type="button" class="button button-highlighted" onclick={openAdd}>
            <Reicon name="plus" class="size-3.5" />
            Add
          </button>
        {/if}
      </div>
    </div>

    {#if !data}
      <div id="environment-table-section" class="application-settings-section-body is-flush mt-1 w-full scroll-mt-28" role="status">
        <span class="sr-only">Loading environment variables</span>
        <SkeletonTable flush rows={6} />
      </div>
    {:else}
      <div id="environment-table-section" class="application-settings-section-body relative mt-1 scroll-mt-28 {total > 0 ? 'is-flush' : ''} w-full">
        {#if search.trim() !== '' && total === 0}
          <Empty size="sm" title="No environment variables found" description="No variables match your search." />
        {:else if total > 0}
          <div class="data-table w-full">
            <div class="environment-table-scroll relative">
              <div class="transition-all">
                <div class="data-table-header env-table-grid env-table-grid-no-type">
                  <span>Name</span>
                  <span class="text-center">Managed</span>
                  <span class="text-center">Literal</span>
                  <span class="text-center">Multiline</span>
                  <span class="text-center">Buildtime</span>
                  <span class="text-center">Runtime</span>
                  <span></span>
                </div>
                {#each rows as row, i (row.row_id)}
                  {#if showPreview && row.scope !== rows[i - 1]?.scope}
                    <div class="flex items-center gap-2 border-b border-neutral-200 bg-neutral-50 px-4 py-2 text-[12px] font-semibold text-neutral-700 dark:border-white/[0.08] dark:bg-white/[0.03] dark:text-fg">
                      {row.scope === 'preview' ? 'Preview deployments' : 'Production'}
                    </div>
                  {/if}
                  {#if row.kind === 'managed'}
                    <EnvRow {kind} {uuid} {row} canUpdate={canManage} availableVars={available} openKey={focusKey} onchange={refresh} />
                  {:else}
                    <EnvHardcodedRow {kind} {uuid} {row} onfocuskey={focusCompose} />
                  {/if}
                {/each}
              </div>
            </div>
            <Pager
              {total}
              bind:page={() => page, (v) => {
                page = v;
                load();
              }}
              bind:pageSize={() => perPage, (v) => {
                perPage = v;
                load();
              }}
              separator="–"
              storageKey="coolify.page-size.environment-variables"
            />
          </div>
        {:else}
          <div class="relative min-h-40">
            <div>
              <Empty size="sm" title="No environment variables" description="Add your first variable with the + Add button above." iconName="variables" />
            </div>
          </div>
        {/if}
      </div>
    {/if}
  {/if}
</div>

{#if adding}
  <Modal title="New Environment Variable" onclose={() => (adding = false)}>
    <form class="flex w-full flex-col gap-4" onsubmit={submitAdd}>
      <div class="grid gap-4 sm:grid-cols-2">
        <FormInput placeholder="NODE_ENV" id="key" label="Name" required bind:value={addKey} error={addError?.field('key')} />
        <FormInput id="comment" label="Comment" placeholder="Optional note" helper="Add a note to document what this environment variable is used for." maxlength={256} bind:value={addComment} error={addError?.field('comment')} />
      </div>
      <div>
        {#if addMultiline}
          <FormTextarea id="value" label="Value" required class="font-sans" bind:value={addValue} />
        {:else}
          <div class="mb-1.5 flex h-4 w-full items-center gap-1.5">
            <label for="value" class="mb-0! flex items-center gap-1 text-sm font-medium leading-4">Value <span class="text-red-500">*</span></label>
          </div>
          <EnvVarInput id="value" placeholder="production" required password={false} bind:value={addValue} availableVars={available} />
          <div class="mt-1.5 text-xs text-neutral-500 dark:text-fg-faint">
            Tip: Type <span class="font-mono text-coollabs dark:text-brand-light">{'{{'}</span> to reference a shared environment variable
          </div>
        {/if}
      </div>
      <div class="grid gap-4 border-t border-neutral-200 pt-4 dark:border-white/[0.07] sm:grid-cols-2">
        <Listbox id="is_multiline" label="Value type" bind:value={addMultiline} options={[{ value: false, label: 'Single line' }, { value: true, label: 'Multiline' }]} />
        <Listbox
          id="is_literal"
          label="Interpolation"
          helper="Literal means $VARIABLES in the value is kept as the actual characters '$VARIABLES' instead of being resolved from another variable. Useful when your value contains a $ sign."
          bind:value={addLiteral}
          options={[{ value: false, label: 'Interpolate $VARIABLES' }, { value: true, label: 'Literal (keep $ characters as-is)' }]}
        />
        <Listbox
          id="is_buildtime"
          label="Build time"
          helper="Make this variable available during the Docker build process. Useful for build secrets and dependencies."
          bind:value={addBuildtime}
          options={[{ value: true, label: 'Available during build' }, { value: false, label: 'Not available during build' }]}
        />
        <Listbox
          id="is_runtime"
          label="Runtime"
          helper="Make this variable available in the running container at runtime."
          bind:value={addRuntime}
          options={[{ value: true, label: 'Available in the container' }, { value: false, label: 'Not available in the container' }]}
        />
      </div>
      {#if addWarning}
        <Callout type="warning" title="Caution">
          <div class="text-sm">{addWarning.issue}</div>
          <div class="text-sm">Recommendation: {addWarning.recommendation}</div>
        </Callout>
      {/if}
      <div class="flex justify-end border-t border-neutral-200 pt-4 dark:border-white/[0.07]">
        <button type="submit" class="button">Add variable</button>
      </div>
    </form>
  </Modal>
{/if}
