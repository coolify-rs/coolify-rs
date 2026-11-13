<script lang="ts">
  // Port of upstream livewire/image-scan/vulnerabilities.blade.php (ImageScan\Vulnerabilities):
  // the Security section of a resource (spec 15.7): the main card with the status, the sentence,
  // and the action that fixes the most, then the findings of the images the resource runs.
  // coolify-rs has no push channel, so the page polls while a scan or a version check runs.
  import { untrack } from 'svelte';
  import Callout from '../Callout.svelte';
  import CheckpointItem from '../CheckpointItem.svelte';
  import Empty from '../Empty.svelte';
  import Pager from '../Pager.svelte';
  import Reicon from '../Reicon.svelte';
  import SettingsSection from '../SettingsSection.svelte';
  import StatusBadge from '../StatusBadge.svelte';
  import { link } from '../../lib/router.svelte';
  import { notify } from '../../lib/toast.svelte';
  import { ApiError } from '../../lib/api';
  import {
    parseSubmit,
    run,
    vulnerabilitiesApi as api,
    type FindingRow as Finding,
    type ListState,
    type PackageGroup,
    type ResourceKind,
    type VulnerabilitiesData,
  } from '../../lib/imageScanApi';
  import CheckMark from './CheckMark.svelte';
  import ErrorLog from './ErrorLog.svelte';
  import ExportMenu from './ExportMenu.svelte';
  import FindingRow from './FindingRow.svelte';
  import IgnoreModal, { type IgnoreTarget } from './IgnoreModal.svelte';
  import ImageSelect from './ImageSelect.svelte';
  import SecurityAction from './SecurityAction.svelte';
  import SecurityImage from './SecurityImage.svelte';
  import SeverityBadge from './SeverityBadge.svelte';
  import TableFilter from './TableFilter.svelte';
  import TableSearch from './TableSearch.svelte';

  let { kind, uuid, onchanged }: { kind: ResourceKind; uuid: string; onchanged?: () => void } = $props();

  const segment = 'h-7 rounded-md px-2.5 text-[12px] font-medium whitespace-nowrap transition-colors';
  const segmentActive = 'bg-white text-black shadow-sm ring-1 ring-neutral-200 dark:bg-white/[0.09] dark:text-fg dark:ring-white/[0.08]';
  const segmentIdle = 'text-neutral-500 hover:text-black dark:text-fg-faint dark:hover:text-fg';
  const muted = 'text-[11px] text-neutral-500 dark:text-fg-faint';

  function storedPageSize(): number {
    try {
      const v = Number(localStorage.getItem('coolify.page-size.image-scan-findings'));
      return v >= 1 && v <= 100 ? v : 10;
    } catch {
      return 10;
    }
  }

  let list = $state<ListState>({
    fix_state: 'fixed',
    severities: [],
    images: [],
    view: 'package',
    search: '',
    show_ignored: false,
    page: 1,
    per_page: untrack(storedPageSize),
  });
  let data = $state<VulnerabilitiesData | null>(null);
  let loading = $state(false);
  let searching = $state(false);
  let scanning = $state(false);
  let ignoring = $state<IgnoreTarget | null>(null);
  /** The open state of each image row (`x-data="{ open }"`), by row key. */
  let openRows = $state<Record<string, boolean>>({});
  /** The open package groups (`x-data="{ open: false }"`). */
  let openGroups = $state<Record<string, boolean>>({});
  let request = 0;

  async function load(quiet = false) {
    const id = ++request;
    if (!quiet) loading = true;
    try {
      const r = await api.render(kind, uuid, $state.snapshot(list) as ListState);
      if (id !== request) return;
      data = r;
      if (r.pagination) list.page = r.pagination.page;
      if (r.fix_state) list.fix_state = r.fix_state;
      if (r.view) list.view = r.view;
    } catch (e) {
      if (!quiet) notify.failure(e instanceof ApiError ? e.message : String(e));
    } finally {
      if (id === request) {
        loading = false;
        searching = false;
      }
    }
  }

  $effect(() => {
    void kind;
    void uuid;
    untrack(() => void load());
  });

  // `ImageScanUpdated`: poll while a scan or a version check runs.
  const busy = $derived(!!data?.scanning || !!data?.card?.checking);
  $effect(() => {
    if (!busy) return;
    const timer = setInterval(() => void load(true), 4000);
    return () => {
      clearInterval(timer);
      // The scan ended: the menus and the other sections show the new results too.
      onchanged?.();
    };
  });

  function set(patch: Partial<ListState>) {
    Object.assign(list, patch);
    void load();
  }

  function setFixState(state: string) {
    set({ fix_state: state, show_ignored: false, page: 1 });
  }
  function toggleSeverity(severity: string) {
    const s = list.severities.includes(severity) ? list.severities.filter((x) => x !== severity) : [...list.severities, severity];
    set({ severities: s, page: 1 });
  }
  function selectImage(image: string) {
    if (image === '') set({ images: [], page: 1 });
    else showImageFindings(image);
  }
  function showImageFindings(image: string | null) {
    if (image !== null && (data?.image_options?.includes(image) || data?.card?.rows.some((r) => r.ref === image))) {
      set({ images: [image], show_ignored: false, page: 1 });
    }
  }

  async function scanNow() {
    scanning = true;
    if (await run(() => api.scanNow(kind, uuid))) await load(true);
    scanning = false;
  }

  async function submitAction(submit: string) {
    const { name, args } = parseSubmit(submit);
    const call =
      name === 'rebuild'
        ? () => api.rebuild(kind, uuid)
        : name === 'updateImages'
          ? () => api.updateImages(kind, uuid, args[0] ?? '')
          : () => api.updateImage(kind, uuid, args[0] ?? '', args[1] ? args[1] : null);
    if (await run(call)) {
      await load(true);
      onchanged?.();
    }
  }

  async function removeOverride(service: string) {
    if (await run(() => api.removeImageOverride(kind, uuid, service))) await load(true);
  }

  async function ignore(body: { target: string; kind: string; scope: string; expires: string; reason: string; owner: string | null }) {
    const r = await api.ignore(kind, uuid, body);
    notify.success(r.message);
    await load(true);
    onchanged?.();
    return r.saved === true;
  }

  const items = $derived(data?.pagination?.items ?? []);
  const grouped = $derived(!list.show_ignored && list.view === 'package');
  const groups = $derived(items as PackageGroup[]);
  const findings = $derived(items as Finding[]);
  const isScanning = $derived(!!data?.scanning);
  const checkNeeded = $derived(data?.card?.status === 'check_needed' && !data?.card?.checking);
</script>

{#if data}
  <div class="flex flex-col gap-6">
    {#if data.off_state === 'server'}
      {#if data.server_url !== null}
        <Empty title="Image scanning is off for {data.server_name}." iconName="shield-alert">
          {#snippet contents()}
            <a class="button normal-case!" href={data?.server_url} use:link>Turn on</a>
          {/snippet}
        </Empty>
      {:else}
        <Empty title="Image scanning is off for {data.server_name}." description="Ask a team admin to turn it on." iconName="shield-alert" />
      {/if}
    {:else if data.off_state === 'application'}
      <Empty title="Image scanning is off for this app." description="Turn on Scan this app in the settings below." iconName="shield-alert" />
    {:else}
      <!-- Main card (spec 15.7, 15.11): the status sentence and the one action first, then the
           why-list, the provenance, and the running image. -->
      <SettingsSection id="security-section" title="Security">
        {#snippet badges()}
          {#if data?.card?.badge}
            <StatusBadge type={data.card.badgeType as 'neutral'} label={data.card.badge} />
          {/if}
        {/snippet}
        {#snippet actions()}
          {#if data?.checked_at}
            <span class={muted} data-tooltip={data.checked_at.title}>Checked {data.checked_at.human}</span>
          {/if}
          {#if data?.can_update}
            <button
              type="button"
              class="button normal-case! {checkNeeded || (!data.card && !data.is_application) ? 'button-highlighted' : ''}"
              onclick={scanNow}
              disabled={isScanning || scanning}
            >
              <Reicon name="refresh" class="size-3.5" />
              Scan now
            </button>
          {/if}
        {/snippet}

        <div class="flex flex-col gap-4">
          {#each data.error_messages ?? [] as error, i (i)}
            <Callout type="danger" title="The last scan could not run">
              {error.message}
              <ErrorLog detail={error.detail} />
            </Callout>
          {/each}

          {#if isScanning}
            <div
              class="flex flex-col divide-y divide-neutral-200 overflow-hidden rounded-lg border border-neutral-200 dark:divide-white/[0.06] dark:border-white/[0.08]"
              role="status"
              aria-live="polite"
            >
              {#each data.checkpoints ?? [] as checkpoint (checkpoint.title)}
                <CheckpointItem title={checkpoint.title} status={checkpoint.status} />
              {/each}
            </div>
          {/if}

          {#if !data.card}
            {#if !isScanning && (data.error_messages ?? []).length === 0}
              <Empty size="sm" title={data.is_application ? 'Not scanned yet. The next deploy scans the image.' : 'Not scanned yet.'} />
            {/if}
          {:else if data.card.single && data.card.rows.length === 1}
            <!-- One image: the open row without the list frame and without a second status (spec 15.11). -->
            <SecurityImage
              row={data.card.rows[0]}
              single
              canUpdate={data.can_update}
              onsubmit={submitAction}
              onshowfindings={(f) => {
                showImageFindings(f);
                window.dispatchEvent(new CustomEvent('open-settings-section', { detail: { id: 'all-findings-section' } }));
              }}
              onremoveoverride={removeOverride}
            />
          {:else}
            {#if data.card.header !== null}
              <p class="text-[15px] font-medium leading-6 text-neutral-950 dark:text-fg">{data.card.header}</p>
            {/if}
            <ul class="flex flex-col divide-y divide-neutral-200 rounded-lg border border-neutral-200 dark:divide-white/[0.06] dark:border-white/[0.08]" aria-label="Images">
              {#each data.card.rows as row, index (row.key)}
                {@const open = openRows[row.key] ?? row.open}
                {@const bodyId = `security-image-${row.key}-body`}
                <li>
                  <div class="flex flex-col gap-2 px-3 py-2.5 sm:flex-row sm:items-center sm:gap-3">
                    <button
                      type="button"
                      class="group flex min-w-0 flex-1 items-center gap-2 rounded-md text-left focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-coollabs dark:focus-visible:outline-brand"
                      onclick={() => (openRows[row.key] = !open)}
                      aria-expanded={open ? 'true' : 'false'}
                      aria-controls={bodyId}
                    >
                      <Reicon
                        name="chevron-down"
                        class="size-3.5 shrink-0 text-neutral-500 transition-transform duration-[180ms] ease-[cubic-bezier(0.23,1,0.32,1)] motion-reduce:transition-none dark:text-fg-faint {open ? '' : '-rotate-90'}"
                      />
                      <span class="flex min-w-0 flex-col">
                        {#if row.name !== null}
                          <span class="truncate text-[13px] font-medium text-neutral-950 dark:text-fg">{row.name}</span>
                          <code class="truncate font-mono text-[11px] text-neutral-500 dark:text-fg-faint" title={row.ref}>{row.ref}</code>
                        {:else}
                          <code class="truncate font-mono text-[12px] text-neutral-900 dark:text-fg" title={row.ref}>{row.ref}</code>
                        {/if}
                      </span>
                    </button>
                    <div class="flex shrink-0 flex-wrap items-center gap-2 pl-5.5 sm:pl-0">
                      {#if row.badge !== null}
                        <StatusBadge type={row.badgeType as 'neutral'} label={row.badge} />
                      {:else}
                        <span class={muted}>{row.status === 'ok' ? 'No action needed' : 'Not checked'}</span>
                      {/if}
                      {#if row.mergedWith !== null}
                        <span class={muted}>Updated with {row.mergedWith}</span>
                      {:else if row.primary !== null}
                        <SecurityAction action={row.primary} primary={index === 0} onsubmit={submitAction} />
                      {/if}
                    </div>
                  </div>
                  <!-- A check only the user can unblock (registry login, daily limit) shows also on a closed row. -->
                  {#each row.versionNotes as versionNote, i (i)}
                    <p class="-mt-1 truncate px-3 pb-2 text-[11px] text-neutral-500 sm:pl-8.5 dark:text-fg-faint" title={versionNote.text}>
                      {versionNote.text}
                      {#if versionNote.link !== null}
                        <a href={versionNote.link} class="underline underline-offset-4 hover:text-neutral-950 dark:hover:text-fg" use:link>Registry logins</a>
                      {/if}
                    </p>
                  {/each}
                  <!-- Height and opacity animate together (grid rows 0fr → 1fr). -->
                  <div
                    id={bodyId}
                    class="grid transition-[grid-template-rows,opacity] duration-[180ms] ease-[cubic-bezier(0.23,1,0.32,1)] motion-reduce:transition-none {open
                      ? 'grid-rows-[1fr] opacity-100'
                      : 'grid-rows-[0fr] opacity-0'}"
                    inert={!open}
                  >
                    <div class="min-h-0 overflow-hidden">
                      <div class="px-3 pb-3 pt-1 sm:pl-8.5">
                        <SecurityImage
                          {row}
                          single={false}
                          canUpdate={data.can_update}
                          onsubmit={submitAction}
                          onshowfindings={(f) => {
                            showImageFindings(f);
                            window.dispatchEvent(new CustomEvent('open-settings-section', { detail: { id: 'all-findings-section' } }));
                          }}
                          onremoveoverride={removeOverride}
                        />
                      </div>
                    </div>
                  </div>
                </li>
              {/each}
            </ul>
          {/if}
        </div>
      </SettingsSection>

      {#if data.has_scans}
        <SettingsSection id="all-findings-section" title="All findings ({data.finding_count_text})" collapsible flush>
          {#snippet actions()}
            <ExportMenu count={data?.shown_count ?? 0} load={(format) => api.export(kind, uuid, $state.snapshot(list) as ListState, format)} />
          {/snippet}

          {#if data.truncated}
            <div class="border-b border-neutral-200 p-3 dark:border-white/[0.08]">
              <Callout type="info" title="Too many results">Only part of the results is shown, with the problems that attackers are using first.</Callout>
            </div>
          {/if}

          <div class="table-toolbar flex flex-col gap-2 sm:flex-row sm:flex-wrap sm:items-center border-b border-neutral-200 p-3 dark:border-white/[0.08]">
            <div class="min-w-0 w-full flex-1 sm:max-w-md">
              <TableSearch
                placeholder="Search by ID or package"
                bind:value={list.search}
                loading={searching}
                onsearch={() => {
                  searching = true;
                  set({ page: 1 });
                }}
              />
            </div>
            <div class="flex flex-wrap items-center gap-2 sm:ml-auto">
              {#if !list.show_ignored}
                <div class="inline-flex max-w-full flex-wrap items-center gap-0.5 rounded-lg bg-neutral-100 p-1 dark:bg-white/[0.04]" role="group" aria-label="Fix state">
                  {#each data.fix_states ?? [] as option (option.value)}
                    <button
                      type="button"
                      onclick={() => setFixState(option.value)}
                      aria-pressed={list.fix_state === option.value ? 'true' : 'false'}
                      class="{segment} {list.fix_state === option.value ? segmentActive : segmentIdle}"
                    >
                      {option.label} <span class="tabular-nums">({(data.state_counts?.[option.value as 'fixed' | 'no_fix'] ?? 0).toLocaleString('en-US')})</span>
                    </button>
                  {/each}
                </div>
              {/if}

              <ImageSelect options={data.image_options ?? []} selected={list.images} onselect={selectImage} />

              <TableFilter activeCount={list.severities.length} onreset={() => set({ severities: [], page: 1 })}>
                <span class="px-2 pb-1 pt-2 text-[10px] font-medium uppercase tracking-wider text-neutral-400 dark:text-fg-faint">Severity</span>
                {#each data.severity_options ?? [] as option (option)}
                  <button
                    type="button"
                    class="listbox-option"
                    role="option"
                    aria-selected={list.severities.includes(option) ? 'true' : 'false'}
                    onclick={() => toggleSeverity(option)}
                  >
                    <span>{option.charAt(0).toUpperCase() + option.slice(1)}</span>
                    <CheckMark selected={list.severities.includes(option)} />
                  </button>
                {/each}
              </TableFilter>

              {#if !list.show_ignored}
                <div class="inline-flex items-center gap-0.5 rounded-lg bg-neutral-100 p-1 dark:bg-white/[0.04]" role="group" aria-label="Group results">
                  {#each [['package', 'By package'], ['cve', 'By CVE']] as [value, label] (value)}
                    <button
                      type="button"
                      onclick={() => set({ view: value, page: 1 })}
                      aria-pressed={list.view === value ? 'true' : 'false'}
                      class="{segment} {list.view === value ? segmentActive : segmentIdle}">{label}</button
                    >
                  {/each}
                </div>
              {/if}
            </div>
          </div>

          {#if list.show_ignored}
            <div class="flex items-center justify-between gap-2 border-b border-neutral-200 px-4 py-2 dark:border-white/[0.08]">
              <span class="text-[12px] font-medium text-neutral-700 dark:text-fg">Ignored</span>
              <button type="button" class="text-[12px] font-medium text-coollabs hover:underline dark:text-brand-light" onclick={() => set({ show_ignored: false, page: 1 })}
                >Back to the results</button
              >
            </div>
          {/if}

          <div class="relative">
            {#if loading}
              <div class="table-loading-overlay absolute inset-0 z-30 flex items-center justify-center bg-white/70 backdrop-blur-[1px] dark:bg-black/20">
                <span class="sr-only">Loading records...</span>
              </div>
            {/if}

            {#if items.length === 0}
              {#if !list.show_ignored && !data.has_filters && list.fix_state === 'fixed'}
                {#if (data.state_counts?.no_fix ?? 0) > 0}
                  <Empty size="sm" title="No finding has a fixed version.">
                    {#snippet contents()}
                      <button type="button" class="button normal-case!" onclick={() => setFixState('no_fix')}>
                        No fix yet ({(data?.state_counts?.no_fix ?? 0).toLocaleString('en-US')})
                      </button>
                    {/snippet}
                  </Empty>
                {:else}
                  <Empty size="sm" title="No finding has a fixed version." />
                {/if}
              {:else if data.has_filters}
                <Empty size="sm" title="No matching findings" description="Try another search, or reset the filters." />
              {:else}
                <Empty size="sm" title={list.show_ignored ? 'No ignored findings.' : 'No findings in this group.'} />
              {/if}
            {:else if grouped}
              <div class="data-table">
                <div class="data-table-header image-scan-packages-grid {data.shows_image ? 'has-image' : ''}">
                  <span>Package</span>
                  {#if data.shows_image}
                    <span class="image-scan-secondary">Image</span>
                  {/if}
                  <span class="image-scan-secondary">Version</span>
                  <span class="image-scan-severity-cell">Severity</span>
                  <span class="image-scan-secondary">Findings</span>
                  <span class="sr-only">Actions</span>
                </div>
                {#each groups as group (group.key)}
                  {@const groupOpen = openGroups[group.key] ?? false}
                  <div class="border-b border-neutral-200 last:border-b-0 dark:border-white/[0.08]">
                    <div class="data-table-row image-scan-packages-grid {data.shows_image ? 'has-image' : ''}">
                      <div class="flex min-w-0 items-center gap-1.5">
                        <button
                          type="button"
                          class="icon-button"
                          onclick={() => (openGroups[group.key] = !groupOpen)}
                          aria-expanded={groupOpen ? 'true' : 'false'}
                          aria-label="Show the findings of {group.package}"
                        >
                          <Reicon name="chevron-down" class="size-3.5 transition-transform duration-150 {groupOpen ? 'rotate-0' : '-rotate-90'}" />
                        </button>
                        <span class="truncate text-[12px] font-medium text-neutral-950 dark:text-fg" title={group.package}>{group.package}</span>
                      </div>
                      {#if data.shows_image}
                        <span class="image-scan-secondary truncate {muted}" title={group.image}>{group.image}</span>
                      {/if}
                      <span
                        class="image-scan-secondary min-w-0 truncate font-mono text-[11px] text-neutral-600 dark:text-fg-dim"
                        title="{group.version}{group.fixed ? ` → ${group.fixed}` : ''}"
                      >
                        {group.version}{#if group.fixed}{` → ${group.fixed}`}{/if}
                      </span>
                      <span class="image-scan-severity-cell flex min-w-0 flex-wrap items-center gap-1">
                        <SeverityBadge severity={group.severity} />
                        {#if group.kev}
                          <span data-tooltip="Attackers are using this, according to CISA.">
                            <StatusBadge type="error" label="Known exploited" />
                          </span>
                        {/if}
                      </span>
                      <span class="image-scan-secondary text-[12px] tabular-nums text-neutral-600 dark:text-fg-dim">{group.count.toLocaleString('en-US')}</span>
                      <span class="flex justify-end">
                        {#if data.can_update}
                          <button
                            type="button"
                            class="icon-button"
                            data-tooltip="Ignore package"
                            aria-label="Ignore {group.package}"
                            onclick={() => (ignoring = { target: group.package, kind: 'package', owner: group.owner, neverAllowed: group.never_allowed })}
                          >
                            <Reicon name="eye-off2" class="size-3.5" />
                          </button>
                        {/if}
                      </span>
                    </div>
                    {#if groupOpen}
                      <div class="border-t border-neutral-200 bg-neutral-50/60 dark:border-white/[0.06] dark:bg-white/[0.015]">
                        {#each group.rows as row (row.key)}
                          <FindingRow {row} nested canUpdate={data.can_update} onignore={(d) => (ignoring = d)} />
                        {/each}
                      </div>
                    {/if}
                  </div>
                {/each}
              </div>
            {:else}
              <div class="data-table">
                <div class="data-table-header image-scan-findings-grid {data.shows_image ? 'has-image' : ''}">
                  <span>Finding</span>
                  <span class="image-scan-secondary">Package</span>
                  {#if data.shows_image}
                    <span class="image-scan-secondary">Image</span>
                  {/if}
                  <span class="image-scan-secondary">Version</span>
                  <span class="image-scan-severity-cell">Severity</span>
                  <span class="sr-only">Actions</span>
                </div>
                {#each findings as row (row.key)}
                  <FindingRow {row} showsImage={data.shows_image} canUpdate={data.can_update} onignore={(d) => (ignoring = d)} />
                {/each}
              </div>
            {/if}
          </div>

          {#if data.grouped_finding_count != null && (data.pagination?.total ?? 0) > 0}
            <p class="border-t border-neutral-200 px-4 py-2.5 {muted} tabular-nums dark:border-white/[0.08]">
              {data.grouped_finding_count === 1 ? '1 finding' : `${data.grouped_finding_count.toLocaleString('en-US')} findings`}
              in {data.pagination?.total === 1 ? '1 package' : `${(data.pagination?.total ?? 0).toLocaleString('en-US')} packages`}
            </p>
          {/if}

          <Pager
            total={data.pagination?.total ?? 0}
            separator="–"
            storageKey="coolify.page-size.image-scan-findings"
            bind:page={() => list.page, (p) => p !== list.page && set({ page: p })}
            bind:pageSize={() => list.per_page, (s) => s !== list.per_page && set({ per_page: s, page: 1 })}
          />

          {#if (data.ignored_count ?? 0) > 0 && !list.show_ignored}
            <div class="border-t border-neutral-200 px-4 py-2.5 dark:border-white/[0.08]">
              <button type="button" class="text-[12px] font-medium text-coollabs hover:underline dark:text-brand-light" onclick={() => set({ show_ignored: true, page: 1 })}
                >{(data.ignored_count ?? 0).toLocaleString('en-US')} ignored</button
              >
            </div>
          {/if}
        </SettingsSection>
      {/if}

      {#if data.can_update && ignoring}
        <IgnoreModal detail={ignoring} canIgnoreForTeam={!!data.can_ignore_for_team} onsubmit={ignore} onclose={() => (ignoring = null)} />
      {/if}
    {/if}
  </div>
{/if}
