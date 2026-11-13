<script lang="ts">
  // Port of upstream livewire/server/security/host-vulnerabilities.blade.php
  // (Server\Security\HostVulnerabilities): the known vulnerabilities in the operating system
  // packages of a server, with the package updates of the last patch check. Polls every 10 s
  // while a scan runs (`wire:poll.10s="loadResult"`).
  import { untrack } from 'svelte';
  import Callout from '../Callout.svelte';
  import ConfirmModal from '../ConfirmModal.svelte';
  import Empty from '../Empty.svelte';
  import Loading from '../Loading.svelte';
  import Pager from '../Pager.svelte';
  import Reicon from '../Reicon.svelte';
  import RemoveUnusedPackagesButton from '../RemoveUnusedPackagesButton.svelte';
  import ServerActivityDialog from '../ServerActivityDialog.svelte';
  import SettingsSection from '../SettingsSection.svelte';
  import SortHeader from '../SortHeader.svelte';
  import StatusBadge from '../StatusBadge.svelte';
  import { request } from '../../lib/api';
  import { action, failure, post } from '../../lib/serverApi';
  import ErrorLog from './ErrorLog.svelte';
  import ExportMenu from './ExportMenu.svelte';
  import SeverityBadge from './SeverityBadge.svelte';
  import TableSearch from './TableSearch.svelte';

  interface HostFinding {
    key: string;
    id: string;
    display_id: string;
    title: string;
    summary: string;
    package: string;
    version: string;
    fixed_in: string | null;
    severity: string;
    signal: 'exploited' | 'likely' | null;
    kev: boolean;
    cvss_score: string | null;
    likelihood: string | null;
    aliases: string[];
    advisory_url: string | null;
    fixing_updates: { package: string; new_version: string }[];
    is_own_update: boolean;
    risk_labels: string[];
    kernel_status: 'fixed_after_reboot' | 'runs_after_reboot' | null;
    is_other_kernel: boolean;
    is_old_kernel: boolean;
  }

  interface HostData {
    name: string;
    can_update: boolean;
    host_scan_running: boolean;
    host_scan_available: boolean;
    host_scan: {
      summary: Record<string, number | string> | null;
      scanned_at: { human: string; title: string } | null;
      findings_truncated: boolean;
      eol: { name?: string; version?: string } | null;
      error_detail: string | null;
    } | null;
    host_scan_error: string | null;
    has_result: boolean;
    reboot_required: boolean;
    package_manager: string | null;
    fixed_after_reboot: number;
    other_kernel_count: number;
    has_old_kernels: boolean;
    running_kernel: string | null;
    boot_kernel: string | null;
    host_finding_count: number;
    shown_count: number;
    findings: HostFinding[];
    pagination: { total: number; page: number; per_page: number; last_page: number };
    sort_by: string;
  }

  let { uuid }: { uuid: string } = $props();

  const url = $derived(`/ui/server-page/${encodeURIComponent(uuid)}/host-vulnerabilities`);
  const patches = $derived(`/server-page/${encodeURIComponent(uuid)}/security/patches`);
  let data = $state<HostData | null>(null);
  let search = $state('');
  let sortBy = $state('');
  let showOtherKernels = $state(false);
  let page = $state(1);
  let perPage = $state(10);
  let searching = $state(false);
  let activityId = $state<number | null>(null);
  let dialog = $state(false);
  let rebooting = $state(false);
  let confirming = $state<HostFinding | null>(null);
  let removeUnusedOpen = $state(false);
  let open = $state<Record<string, boolean>>({});

  const listState = () => ({ search, sort_by: sortBy, show_other_kernels: showOtherKernels, page, per_page: perPage });

  async function load() {
    try {
      const r = await request<HostData>('POST', url, listState());
      data = r;
      page = r.pagination.page;
    } catch (e) {
      failure(e);
    } finally {
      searching = false;
    }
  }

  $effect(() => {
    void uuid;
    untrack(() => void load());
  });
  $effect(() => {
    if (!data?.host_scan_running) return;
    const timer = setInterval(() => void load(), 10000);
    return () => clearInterval(timer);
  });

  async function scan() {
    try {
      const r = await request<{ host_scan_running: boolean }>('POST', `${url}/scan`, {});
      if (data) data.host_scan_running = r.host_scan_running;
    } catch (e) {
      failure(e);
    }
  }

  /** `runUpdate()`: the update logs open in the process dialog. */
  async function start(path: string, body: unknown = {}) {
    try {
      const r = await post<{ activity_id: number }>(path, body);
      activityId = r.activity_id;
      dialog = true;
    } catch (e) {
      failure(e);
    }
  }

  const summary = $derived(data?.host_scan?.summary ?? null);
  const num = (key: string) => Number(summary?.[key] ?? 0);
  const noRelevant = $derived(!!summary && ['critical', 'high', 'medium', 'low', 'unknown'].reduce((t, k) => t + num(k), 0) === 0);
  const plural = (n: number) => (n === 1 ? 'vulnerability' : 'vulnerabilities');
  const exportLoad = (format: string) => request<{ content: string; name?: string; mime?: string }>('POST', `${url}/export`, { ...listState(), format });
</script>

{#if dialog}
  <ServerActivityDialog {uuid} {activityId} title="Package Update Logs" onclose={() => ((dialog = false), void load())} />
{/if}

{#if data}
  <SettingsSection
    id="server-host-vulnerabilities-section"
    title="Vulnerabilities"
    description="Known vulnerabilities in the operating system packages of this server. Coolify scans them with the image scanner every day and after a package update. Problems that the distribution does not fix are not shown."
    flush
  >
    {#snippet badges()}
      {#if data?.host_scan_error && !data.host_scan_running}
        <StatusBadge label="Scan failed" type="error" />
      {/if}
      {#if summary}
        {#each ['critical', 'high'] as severity (severity)}
          {#if num(severity) > 0}
            <SeverityBadge {severity} count={num(severity)} />
          {/if}
        {/each}
        {#if num('exploited') > 0}
          <StatusBadge label="{num('exploited')} exploited" type="error" />
        {/if}
        {#if noRelevant}
          <StatusBadge label="No relevant problems" type="success" />
        {/if}
      {:else if !data?.host_scan_available}
        <StatusBadge label="Off" type="neutral" />
      {:else if !data?.host_scan_running}
        <StatusBadge label="Not scanned yet" type="neutral" />
      {/if}
    {/snippet}
    {#snippet actions()}
      {#if data && (data.host_finding_count > 0 || data.host_scan_available)}
        {#if data.host_finding_count > 0}
          <div class="w-56">
            <TableSearch
              placeholder="Search by ID or package"
              bind:value={search}
              loading={searching}
              onsearch={() => {
                searching = true;
                page = 1;
                void load();
              }}
            />
          </div>
          <ExportMenu count={data.shown_count} load={exportLoad} />
        {/if}
        {#if data.host_scan_available && data.can_update}
          <button type="button" class="button" onclick={scan} disabled={data.host_scan_running}>
            {#if data.host_scan_running}
              <Loading compact />
              Scanning…
            {:else}
              <Reicon name="refresh" class="size-3.5" />
              Scan now
            {/if}
          </button>
        {/if}
      {/if}
    {/snippet}

    {#if !data.host_scan_available && !data.host_scan}
      <div class="p-4">
        <Empty
          size="sm"
          iconName="shield-alert"
          title="Image scanning is off for {data.name}."
          description="Enable image scanning below to also scan the operating system packages of this server."
        >
          {#snippet contents()}
            <a class="text-[13px] font-medium underline" href="#server-image-scanning-section">Image scanning settings</a>
          {/snippet}
        </Empty>
      </div>
    {:else}
      <div class="flex flex-col gap-3 p-4">
        <div class="flex flex-wrap items-center gap-2 text-sm text-neutral-600 dark:text-fg-dim">
          <span>
            Last scan:
            {#if data.host_scan?.scanned_at}
              <span title={data.host_scan.scanned_at.title}>{data.host_scan.scanned_at.human}</span>
            {:else}
              never
            {/if}
          </span>
          {#if summary}
            {#if num('medium') > 0}
              <SeverityBadge severity="medium" count={num('medium')} />
            {/if}
            {#if num('fixable') > 0}
              <StatusBadge label="{num('fixable')} fixed by an update" type="warning" />
            {/if}
          {/if}
        </div>
        {#if data.host_finding_count > 0 && (data.host_scan?.findings_truncated || !data.has_result)}
          <p class="text-[12px] text-neutral-500 dark:text-fg-faint">
            {#if data.host_scan?.findings_truncated}
              Only the most important vulnerabilities of a larger result are shown.
            {/if}
            {#if !data.has_result}
              Run "Check for updates" to see which packages have an update.
            {/if}
          </p>
        {/if}

        {#if data.reboot_required}
          <Callout type="warning" title="Reboot required">
            <div class="flex flex-wrap items-center justify-between gap-3">
              <span>
                {#if data.fixed_after_reboot > 0}
                  The server runs kernel <span class="font-mono">{data.running_kernel}</span>, which has {data.fixed_after_reboot}
                  listed {plural(data.fixed_after_reboot)}. The fixed kernel
                  <span class="font-mono">{data.boot_kernel}</span> is installed and is the boot kernel. Reboot the server to start it.
                {:else if data.running_kernel && data.boot_kernel && data.boot_kernel !== data.running_kernel}
                  Kernel <span class="font-mono">{data.boot_kernel}</span> is installed and starts after a reboot. The server now runs
                  <span class="font-mono">{data.running_kernel}</span>. No listed vulnerability needs this reboot.
                {:else}
                  Some installed updates apply only after a reboot.
                {/if}
              </span>
              {#if data.can_update}
                <button type="button" class="button button-error" onclick={() => (rebooting = true)}>Reboot server</button>
              {/if}
            </div>
          </Callout>
        {/if}
        {#if data.other_kernel_count > 0}
          <div class="flex flex-wrap items-center gap-x-3 gap-y-2 text-[12px] text-neutral-500 dark:text-fg-faint">
            <span>{data.other_kernel_count} {plural(data.other_kernel_count)} in installed kernels that do not run {showOtherKernels ? 'shown' : 'hidden'}. No risk now.</span>
            <button
              type="button"
              onclick={() => {
                showOtherKernels = !showOtherKernels;
                page = 1;
                void load();
              }}
              class="cursor-pointer font-medium underline underline-offset-4"
            >
              {showOtherKernels ? 'Hide' : 'Show'}
            </button>
            {#if data.has_old_kernels && data.can_update}
              <RemoveUnusedPackagesButton
                packageManager={data.package_manager}
                buttonTitle="Remove old kernels"
                bind:open={removeUnusedOpen}
                onconfirm={() => start(`${patches}/remove-unused`)}
              />
            {/if}
          </div>
        {/if}
        {#if data.host_scan_error}
          <Callout type="danger" title="The last scan failed">
            {data.host_scan_error}
            {#if data.can_update}
              <ErrorLog detail={data.host_scan?.error_detail ?? null} />
            {/if}
          </Callout>
        {/if}
        {#if data.host_scan?.eol}
          <Callout type="warning" title="Operating system is end of life">
            {data.host_scan.eol.name ?? ''}
            {data.host_scan.eol.version ?? ''} gets no security updates anymore. Upgrade the operating system of this server.
          </Callout>
        {/if}
        {#if data.host_scan?.scanned_at && data.host_finding_count === 0}
          <Empty
            size="sm"
            title="No relevant vulnerabilities"
            description="The installed packages have no critical, high, or exploited vulnerabilities."
            iconName="check-circle"
          />
        {/if}
      </div>

      {#if data.host_finding_count > 0 && data.findings.length === 0}
        <div class="border-t border-neutral-200 p-6 dark:border-white/[0.08]">
          <Empty size="sm" title="No matching vulnerabilities" description="Try another ID or package, or clear the search." iconName="search" />
        </div>
      {:else if data.host_finding_count > 0}
        <div class="data-table">
          <div class="data-table-header host-vulnerabilities-table-grid">
            {#each [['Vulnerability', 'vulnerability', 'asc'], ['Package', 'package', 'asc'], ['Fixed in', 'fixed', 'asc']] as [label, column] (column)}
              <SortHeader
                {label}
                asc="{column}_asc"
                desc="{column}_desc"
                current={sortBy}
                server
                onsort={(v) => {
                  sortBy = v;
                  page = 1;
                  void load();
                }}
              />
            {/each}
            <div class="flex justify-end">
              <SortHeader
                label="Severity"
                asc="severity_asc"
                desc="severity_desc"
                first="desc"
                current={sortBy}
                server
                onsort={(v) => {
                  sortBy = v;
                  page = 1;
                  void load();
                }}
              />
            </div>
            <span class="text-right">Action</span>
          </div>
          {#each data.findings as finding (finding.key)}
            {@const isOpen = open[finding.key] ?? false}
            <div class="border-b border-neutral-200 last:border-b-0 dark:border-white/[0.08]">
              <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
              <div
                onclick={() => (open[finding.key] = !isOpen)}
                class="data-table-row host-vulnerabilities-table-grid w-full cursor-pointer text-left hover:bg-neutral-50 dark:hover:bg-white/[0.03]"
              >
                <div class="flex min-w-0 items-start gap-1.5">
                  <Reicon
                    name="chevron-down"
                    class="mt-0.5 size-3.5 shrink-0 text-neutral-500 transition-transform duration-150 dark:text-fg-faint {isOpen ? 'rotate-0' : '-rotate-90'}"
                  />
                  <div class="min-w-0">
                    <p class="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1">
                      <button
                        type="button"
                        onclick={(event) => {
                          event.stopPropagation();
                          open[finding.key] = !isOpen;
                        }}
                        aria-expanded={isOpen ? 'true' : 'false'}
                        aria-label="Show details of {finding.display_id}"
                        class="shrink-0 cursor-pointer font-mono text-[12px] text-neutral-950 dark:text-fg">{finding.display_id}</button
                      >
                      {#if finding.signal === 'exploited'}
                        <StatusBadge label="Exploited" type="error" />
                      {:else if finding.signal === 'likely'}
                        <StatusBadge label="Likely exploited" type="warning" />
                      {/if}
                    </p>
                    {#if finding.title}
                      <p class="mt-0.5 truncate text-[11px] text-neutral-500 dark:text-fg-faint">{finding.title}</p>
                    {/if}
                  </div>
                </div>
                <div class="min-w-0">
                  <p class="truncate font-mono text-[12px] text-neutral-700 dark:text-fg-dim">{finding.package}</p>
                  <p class="mt-0.5 truncate text-[10px] text-neutral-500 dark:text-fg-faint">Installed: {finding.version}</p>
                </div>
                <div class="flex min-w-0 flex-col items-start gap-1">
                  <span class="truncate font-mono text-[12px] text-neutral-700 dark:text-fg-dim">{finding.fixed_in ?? 'No fix yet'}</span>
                  {#if finding.kernel_status === 'fixed_after_reboot'}
                    <StatusBadge label="Fix installed, reboot needed" type="warning" />
                  {:else if finding.is_old_kernel}
                    <StatusBadge label="Kernel not running" type="neutral" />
                  {:else if finding.kernel_status === 'runs_after_reboot'}
                    <StatusBadge label="Runs after the reboot" type="neutral" />
                  {/if}
                  {#if finding.fixing_updates.length > 0}
                    <StatusBadge label={finding.is_own_update ? `Update available: ${finding.fixing_updates[0].new_version}` : 'Kernel update available'} type="success" />
                  {/if}
                  {#each finding.risk_labels as riskLabel (riskLabel)}
                    <span class="flex items-center gap-1 text-[10px] text-neutral-500 dark:text-fg-faint">
                      <Reicon name="alert-triangle" class="size-3 shrink-0 text-red-500" />
                      {riskLabel}
                    </span>
                  {/each}
                </div>
                <div class="flex justify-end">
                  <SeverityBadge severity={finding.severity} />
                </div>
                <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
                <div class="flex justify-end" onclick={(event) => event.stopPropagation()}>
                  {#if finding.fixing_updates.length > 0 && data.can_update}
                    {#if finding.risk_labels.length > 0}
                      <button type="button" class="button" onclick={() => (confirming = finding)}>Update</button>
                    {:else}
                      <button type="button" class="button" onclick={() => start(`${url.replace(/^\/ui/, '')}/update-package`, { package: finding.package })}>
                        Update
                      </button>
                    {/if}
                  {/if}
                </div>
              </div>
              {#if isOpen}
                <dl
                  class="grid grid-cols-[6.5rem_minmax(0,1fr)] gap-x-3 gap-y-1.5 px-4 pb-4 sm:grid-cols-[8rem_minmax(0,1fr)] sm:pl-9 text-[12px] text-neutral-700 dark:text-fg-dim"
                >
                  {#if finding.summary}
                    <dd class="col-span-2 mb-1 max-w-3xl leading-5">{finding.summary}</dd>
                  {/if}
                  <dt class="text-neutral-500 dark:text-fg-faint">Package</dt>
                  <dd class="min-w-0 break-all font-mono">{finding.package}</dd>
                  <dt class="text-neutral-500 dark:text-fg-faint">Installed</dt>
                  <dd class="min-w-0 break-all font-mono">{finding.version}</dd>
                  <dt class="text-neutral-500 dark:text-fg-faint">Fixed in</dt>
                  <dd class="min-w-0 break-all font-mono">{finding.fixed_in ?? 'No fix yet'}</dd>
                  {#if finding.fixing_updates.length > 0}
                    <dt class="text-neutral-500 dark:text-fg-faint">Update</dt>
                    <dd class="min-w-0 break-words">
                      {#each finding.fixing_updates as update, i (i)}
                        <span class="block break-all font-mono">{update.package} {update.new_version}</span>
                      {/each}
                      {#if finding.risk_labels.length > 0}
                        <span class="block">{finding.risk_labels.join('. ')}.</span>
                      {/if}
                    </dd>
                  {/if}
                  {#if finding.kernel_status !== null || finding.is_other_kernel}
                    <dt class="text-neutral-500 dark:text-fg-faint">Kernel</dt>
                    <dd class="min-w-0 break-words">
                      {#if finding.kernel_status === 'fixed_after_reboot'}
                        The server runs kernel <span class="font-mono">{data.running_kernel}</span>. The kernel that starts after a reboot has the fix. Reboot the server to use it.
                      {:else if finding.is_old_kernel}
                        No risk now: the server does not run this kernel (it runs <span class="font-mono">{data.running_kernel}</span>).
                      {:else}
                        This kernel is installed, but the server runs it only after a reboot.
                      {/if}
                    </dd>
                  {/if}
                  {#if finding.cvss_score !== null}
                    <dt class="text-neutral-500 dark:text-fg-faint">CVSS score</dt>
                    <dd>{finding.cvss_score}</dd>
                  {/if}
                  {#if finding.kev}
                    <dt class="text-neutral-500 dark:text-fg-faint">Exploited</dt>
                    <dd>Attackers are using this, according to CISA.</dd>
                  {:else if finding.likelihood !== null}
                    <dt class="text-neutral-500 dark:text-fg-faint">Exploit likelihood</dt>
                    <dd>{finding.likelihood}</dd>
                  {/if}
                  {#if finding.aliases.length > 0}
                    <dt class="text-neutral-500 dark:text-fg-faint">Also known as</dt>
                    <dd class="min-w-0 break-all font-mono">{finding.aliases.join(', ')}</dd>
                  {/if}
                  {#if finding.advisory_url !== null}
                    <dt class="text-neutral-500 dark:text-fg-faint">Advisory</dt>
                    <dd><a href={finding.advisory_url} target="_blank" rel="noopener noreferrer" class="font-medium underline underline-offset-4">Open {finding.display_id}</a></dd>
                  {/if}
                </dl>
              {/if}
            </div>
          {/each}
          <Pager
            total={data.pagination.total}
            separator="–"
            storageKey="coolify.page-size.server-host-vulnerabilities"
            bind:page={() => page, (p) => p !== page && ((page = p), void load())}
            bind:pageSize={() => perPage, (s) => s !== perPage && ((perPage = s), (page = 1), void load())}
          />
        </div>
      {/if}
    {/if}
  </SettingsSection>
{/if}

{#if confirming}
  {@const finding = confirming}
  <ConfirmModal
    title="Update {finding.is_own_update ? finding.package : 'the kernel'}?"
    actions={[`Coolify updates ${finding.fixing_updates.map((u) => u.package).join(', ')}.`, ...finding.risk_labels]}
    warningMessage="Coolify cannot roll back a package update. Check the update logs when it is done."
    confirmWithText={false}
    confirmWithPassword={false}
    step2ButtonText="Update"
    onconfirm={async () => {
      await start(`${url.replace(/^\/ui/, '')}/update-package`, { package: finding.package });
    }}
    onclose={() => (confirming = null)}
  />
{/if}

{#if rebooting && data}
  <ConfirmModal
    title="Reboot server?"
    warningMessage="The containers on this server stop until the server is back."
    actions={[
      'The server reboots in a few seconds.',
      'All containers on this server stop, and the containers with a restart policy start again after the reboot.',
      'The server is not reachable for a few minutes.',
    ]}
    confirmationText={data.name}
    confirmationLabel="Enter the server name to confirm"
    shortConfirmationLabel="Server name"
    confirmWithPassword={false}
    step2ButtonText="Reboot server"
    onconfirm={async () => {
      await action(() => post(`${patches}/reboot`));
    }}
    onclose={() => (rebooting = false)}
  />
{/if}
