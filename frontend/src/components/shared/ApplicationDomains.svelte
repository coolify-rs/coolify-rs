<script lang="ts">
  // Port of upstream livewire/project/application/domains.blade.php and
  // partials/domain-row.blade.php (Project\Application\Domains).
  import Callout from '../Callout.svelte';
  import ConfirmModal from '../ConfirmModal.svelte';
  import Empty from '../Empty.svelte';
  import DnsMenu from './DnsMenu.svelte';
  import { followDnsJobs, hostOf, type AfterAddDns } from '../../lib/dnsAfterAdd';
  import Listbox from '../Listbox.svelte';
  import Modal from '../Modal.svelte';
  import Reicon from '../Reicon.svelte';
  import ServiceDomainConflict from '../ServiceDomainConflict.svelte';
  import SettingsSection from '../SettingsSection.svelte';
  import StatusBadge from '../StatusBadge.svelte';
  import { ApiError, request } from '../../lib/api';
  import { link } from '../../lib/router.svelte';
  import { resPath } from '../../lib/sharedApi';
  import { notify } from '../../lib/toast.svelte';

  let { uuid, base, onchange }: { uuid: string; base: string; onchange?: () => void } = $props();

  interface Parts {
    scheme: string;
    host: string;
    port: string;
    path: string;
  }
  interface Row {
    url: string;
    service: string | null;
    public_url: string;
    internal_port: number | null;
    has_port_override: boolean;
    inherited_port: number | null;
    dns_status: string;
    dns_message: string;
    is_noindexed: boolean;
    redirect: string;
    editing_parts: Parts;
  }
  interface Data {
    can_update: boolean;
    is_compose: boolean;
    labels_are_writable: boolean;
    compose_services: string[];
    force_https: boolean;
    permanent_redirect: boolean;
    rows: Row[];
  }
  interface Answer {
    message?: string;
    warnings?: string[];
    domain_conflicts?: { domain: string; resource_name: string; resource_link?: string | null; resource_type: string }[];
    port_warning?: number;
    dns?: AfterAddDns;
  }
  type Force = { force_domains?: boolean; force_port?: boolean };

  const empty = (): Parts => ({ scheme: 'https', host: '', port: '', path: '' });

  let data = $state<Data | null>(null);
  let search = $state('');
  let addOpen = $state(false);
  let newService = $state('');
  let newParts = $state<Parts>(empty());
  let addError = $state('');
  let editing = $state<Row | null>(null);
  let editParts = $state<Parts>(empty());
  let editIndexing = $state<'index' | 'noindex'>('index');
  let editRedirect = $state('both');
  let originalRedirect = 'both';
  let generatedHost = '';
  let editError = $state('');
  let removing = $state<Row | null>(null);
  let conflicts = $state<Answer['domain_conflicts'] | null>(null);
  let unknownPort = $state<number | null>(null);
  let pending: ((force: Force) => Promise<void>) | null = null;
  let forces: Force = {};

  const path = $derived(`${resPath('application', uuid)}/domains`);
  const message = (e: unknown) => (e instanceof ApiError ? e.message : String(e));

  async function load() {
    data = await request<Data>('GET', path);
    if (!newService && data.compose_services.length > 0) newService = data.compose_services[0];
  }

  $effect(() => {
    load().catch((e) => notify.failure(message(e)));
  });

  const rows = $derived(data?.rows ?? []);
  const canUpdate = $derived(!!data?.can_update);
  const hasRows = $derived(rows.length > 0);
  const hasHttps = $derived(rows.some((r) => r.url.toLowerCase().startsWith('https://')));
  const matches = (value: string) => !search.trim() || value.toLowerCase().includes(search.trim().toLowerCase());
  // `$serviceOrder`.
  const groups = $derived.by(() => {
    if (!data?.is_compose) return [];
    const names = data.compose_services.filter((n) => rows.some((r) => r.service === n));
    for (const r of rows) if (r.service && !names.includes(r.service)) names.push(r.service);
    return names.map((name) => ({ name, rows: rows.filter((r) => r.service === name) }));
  });

  let dnsMenu = $state<DnsMenu>();
  // `markDnsPending`: on the page only, a pending status is not stored.
  let dnsPending = $state<{ hosts: string[]; message: string }>({ hosts: [], message: '' });
  const dnsTitle = (row: { url: string; dns_status: string; dns_message: string }) =>
    row.dns_status === 'ok' ? undefined : row.dns_status === 'pending' && dnsPending.hosts.includes(hostOf(row.url)) ? dnsPending.message : row.dns_message;
  const dnsType = (s: string) => (s === 'ok' ? 'success' : s === 'failed' ? 'error' : s === 'skipped' ? 'warning' : 'neutral');
  const dnsLabel = (s: string) =>
    s === 'ok' ? 'DNS matches' : s === 'failed' ? 'DNS mismatch' : s === 'skipped' ? 'DNS skipped' : s === 'checking' ? 'Checking DNS...' : s === 'pending' ? 'Not checked' : 'DNS unknown';
  async function checkDns(row: { url: string; service: string | null }) {
    try {
      await request('POST', `${resPath('application', uuid)}/dns/check`, { url: row.url, service: row.service });
      await load();
    } catch (e) {
      notify.failure(e instanceof ApiError ? e.message : String(e));
    }
  }
  // `pollDnsChecks`: while a check runs, and its toast when it ends.
  let checkingNow = new Set<string>();
  $effect(() => {
    const rows = data?.rows ?? [];
    if (!rows.some((r: { dns_status: string }) => r.dns_status === 'checking')) return;
    const timer = setTimeout(load, 2000);
    return () => clearTimeout(timer);
  });
  $effect(() => {
    const now = new Set<string>();
    for (const r of data?.rows ?? []) {
      const key = `${r.service ?? ''}|${r.url}`;
      if (r.dns_status === 'checking') now.add(key);
      else if (checkingNow.has(key)) {
        let host = r.url;
        try { host = new URL(r.url).hostname; } catch { /* keep the URL */ }
        if (r.dns_status === 'ok') notify.success(`DNS is configured correctly for ${host}.`);
        else if (r.dns_status === 'failed') notify.failure(`DNS is not configured for ${host}. Review the required DNS record. If you changed it recently, DNS propagation can take some time, so please try again later.`);
        else notify.info(`DNS check skipped for ${host}.`);
      }
    }
    checkingNow = now;
  });

  async function handle(call: () => Promise<Answer>, done: () => void, setError: (m: string) => void, retry: (force: Force) => Promise<void>) {
    setError('');
    try {
      const r = await call();
      if (r.domain_conflicts) {
        conflicts = r.domain_conflicts;
        pending = retry;
        return;
      }
      if (r.port_warning) {
        unknownPort = r.port_warning;
        pending = retry;
        return;
      }
      forces = {};
      for (const w of r.warnings ?? []) notify.warning(w);
      for (const i of r.dns?.info ?? []) notify.info(i);
      if (r.message) notify.success(r.message);
      done();
      await load();
      onchange?.();
      if (r.dns) {
        dnsPending = { hosts: r.dns.pending ?? [], message: r.dns.pending_message ?? '' };
        if (r.dns.open_modal) void dnsMenu?.openProviders();
        if (r.dns.jobs?.length) followDnsJobs('application', uuid, r.dns.jobs, load);
      }
    } catch (e) {
      if (e instanceof ApiError && e.errors.domain?.length) setError(e.errors.domain[0]);
      else notify.failure(message(e));
    }
  }

  function submitAdd(force: Force = {}): Promise<void> {
    forces = { ...forces, ...force };
    return handle(
      () => request<Answer>('POST', `${path}/add`, { service: newService, ...newParts, ...forces }),
      () => {
        addOpen = false;
        newParts = empty();
      },
      (m) => (addError = m),
      submitAdd,
    );
  }

  function submitEdit(force: Force = {}): Promise<void> {
    if (!editing) return Promise.resolve();
    const row = editing;
    forces = { ...forces, ...force };
    return handle(
      () =>
        request<Answer>('POST', `${path}/update`, {
          old_url: row.url,
          service: row.service,
          ...editParts,
          indexing: editIndexing,
          redirect: editRedirect,
          original_redirect: originalRedirect,
          generated_host: generatedHost,
          ...forces,
        }),
      () => (editing = null),
      (m) => (editError = m),
      submitEdit,
    );
  }

  function openEdit(row: Row) {
    editing = row;
    editParts = { ...row.editing_parts };
    editIndexing = row.is_noindexed ? 'noindex' : 'index';
    editRedirect = row.redirect;
    originalRedirect = row.redirect;
    generatedHost = '';
    editError = '';
  }

  async function generate() {
    await handle(
      () => request<Answer>('POST', `${path}/generate`, { service: newService }),
      () => {
        addOpen = false;
        newParts = empty();
      },
      (m) => (addError = m),
      async () => {},
    );
  }

  async function regenerate() {
    try {
      const { host } = await request<{ host: string }>('POST', `${path}/regenerate`, {});
      generatedHost = host;
      editParts = { ...editParts, host: editParts.host.toLowerCase().startsWith('www.') ? `www.${host}` : host };
      editError = '';
    } catch (e) {
      notify.failure(message(e));
    }
  }

  async function saveSetting(body: { force_https?: boolean; permanent_redirect?: boolean }) {
    try {
      notify.success((await request<{ message: string }>('POST', `${path}/settings`, body)).message);
    } catch (e) {
      notify.failure(message(e));
    }
    await load();
    onchange?.();
  }

  async function remove(row: Row, deleteManagedDns = false) {
    try {
      const r = await request<Answer>('POST', `${path}/remove`, { url: row.url, service: row.service, delete_managed_dns: deleteManagedDns });
      for (const w of r.warnings ?? []) notify.warning(w);
      if (r.message) notify.success(r.message);
    } catch (e) {
      notify.failure(message(e));
    }
    await load();
    onchange?.();
  }
</script>

{#snippet domainInput(prefix: string, parts: Parts, error: string, inherited: number | null | undefined)}
  <!-- components/forms/domain-input.blade.php -->
  <div class="grid gap-4 sm:grid-cols-[8rem_minmax(0,1fr)_8rem]">
    <div class="min-w-0">
      <Listbox
        id="{prefix}-protocol"
        label="Protocol"
        bind:value={parts.scheme}
        options={[
          { value: 'https', label: 'https' },
          { value: 'http', label: 'http' },
        ]}
      />
    </div>
    <div class="min-w-0">
      <div class="mb-1.5 flex h-4 w-full items-center gap-1.5">
        <label for="{prefix}-host" class="mb-0! flex items-center gap-1.5 leading-4">Domain <span class="text-warning">*</span></label>
      </div>
      <input id="{prefix}-host" type="text" class="input" bind:value={parts.host} placeholder="app.example.com" autocomplete="off" required />
      {#if error}<p class="mt-1 text-[12px] text-red-500">{error}</p>{/if}
    </div>
    <div class="min-w-0">
      <div class="mb-1.5 flex h-4 w-full items-center gap-1.5">
        <label for="{prefix}-port" class="mb-0! flex items-center gap-1.5 leading-4">Port</label>
      </div>
      <input id="{prefix}-port" type="number" class="input" bind:value={parts.port} placeholder={inherited === undefined ? '3000' : String(inherited ?? '')} min="1" max="65535" inputmode="numeric" />
    </div>
    {#if inherited !== undefined && !inherited && !parts.port}
      <div class="min-w-0 sm:col-span-3">
        <Callout type="danger" title="Port is required">The proxy cannot route this domain. Enter the port your container listens on in the Port field.</Callout>
      </div>
    {/if}
    <div class="min-w-0 sm:col-span-3">
      <div class="mb-1.5 flex h-4 w-full items-center gap-1.5">
        <label for="{prefix}-path" class="mb-0! flex items-center gap-1.5 leading-4">Path</label>
      </div>
      <input id="{prefix}-path" type="text" class="input" bind:value={parts.path} placeholder="/api/v3" autocomplete="off" />
      <p class="mt-1 text-[12px] text-neutral-500 dark:text-fg-dim">Optional path, query, or fragment appended after the domain and port.</p>
    </div>
  </div>
{/snippet}

{#snippet domainRow(row: Row)}
  {@const forced = row.url.startsWith('https://') && !!data?.force_https}
  <div class="env-table-item" class:hidden={!matches(`${row.service ?? ''} ${row.url}`)}>
    <div class="data-table-row service-domains-overview-grid">
      <div class="flex min-w-0 flex-col gap-1">
        <div class="flex min-w-0 items-center gap-2">
          <a
            href={row.public_url}
            target="_blank"
            rel="noopener noreferrer"
            class="min-w-0 flex-1 truncate text-[13px] text-black underline decoration-neutral-300 underline-offset-2 hover:decoration-coollabs dark:text-fg dark:decoration-white/20"
            title={row.public_url}>{row.public_url}</a
          >
        </div>
      </div>
      <div class="service-domain-detail" title="Protocol redirect">
        <span class="service-domain-detail-label">Protocol redirect</span>
        <span>{forced ? 'HTTP → HTTPS' : 'Disabled'}</span>
      </div>
      <div class="service-domain-detail" title="Domain redirect">
        <span class="service-domain-detail-label">Domain redirect</span>
        <span>{row.redirect === 'www' ? 'non-www → www' : row.redirect === 'non-www' ? 'www → non-www' : 'Disabled'}</span>
      </div>
      <div class="service-domain-detail" title={row.has_port_override ? 'Custom internal port for this domain' : 'Inherited from the application or Compose service port'}>
        <span class="service-domain-detail-label">Internal port</span>
        {#if row.internal_port}
          <span aria-label="Internal port {row.internal_port}">{row.internal_port}</span>
        {:else}
          <span role="img" aria-label="No internal port" title="No internal port. Set Ports Exposes or a per-domain internal port so the proxy can route this domain." class="text-red-500 dark:text-red-400">
            <Reicon name="alert-triangle" class="size-4" />
          </span>
        {/if}
      </div>
      <div class="service-domain-detail">
        <span class="service-domain-detail-label">Search indexing</span>
        <span role="img" aria-label={row.is_noindexed ? 'Search indexing blocked' : 'Search indexing allowed'} title={row.is_noindexed ? 'Search indexing blocked' : 'Search indexing allowed'}>
          <Reicon name={row.is_noindexed ? 'x' : 'check'} class="size-4" />
        </span>
      </div>
      <div class="service-domain-mobile-summary" aria-label="Domain routing summary">
        {#if forced}<span>HTTP → HTTPS</span>{/if}
        {#if row.redirect === 'www' || row.redirect === 'non-www'}
          <span>{row.redirect === 'www' ? 'non-www → www' : 'www → non-www'}</span>
        {:else if !forced}
          <span>No redirects</span>
        {/if}
        <span>Port {row.internal_port ?? 'missing'}</span>
        <span>{row.is_noindexed ? 'Noindex' : 'Indexable'}</span>
      </div>
      <div class="service-domain-dns flex min-w-0 items-center">
        <span title={dnsTitle(row)}>
          <StatusBadge label={dnsLabel(row.dns_status)} type={dnsType(row.dns_status)} />
        </span>
      </div>
      <div class="service-domain-actions flex items-center justify-end gap-1">
        {#if canUpdate}
          <button type="button" class="icon-button shrink-0" title="Check DNS" aria-label="Check DNS" onclick={() => checkDns(row)}>
            <Reicon name="refresh" class="size-3.5" />
          </button>
          {#if !data?.labels_are_writable}
            <button type="button" class="icon-button shrink-0" title="Domain settings" aria-label="Settings for {row.public_url}" onclick={() => openEdit(row)}>
              <Reicon name="settings" class="size-3.5" />
            </button>
            <button
              type="button"
              class="icon-button shrink-0 text-red-500 hover:text-red-600 dark:text-red-400 dark:hover:text-red-300"
              title="Remove domain"
              aria-label="Remove domain"
              onclick={() => (removing = row)}
            >
              <Reicon name="trash" class="size-3.5" />
            </button>
          {/if}
        {/if}
      </div>
    </div>
  </div>
{/snippet}

{#if data}
  <div id="application-domains-section" class="domains-overview-container flex flex-col gap-4">
    {#if data.labels_are_writable}
      <Callout type="warning" title="Domains managed via labels" class="mb-4">
        Container label readonly mode is disabled. Domains must be set in the Labels section on the General page.
        {#if !data.is_compose}
          <a class="mt-2 block font-medium underline" use:link href="{base}#container-labels-section">Go to Container labels</a>
        {/if}
      </Callout>
    {/if}

    {#if data.is_compose && data.compose_services.length === 0}
      <Callout type="info" title="No services">No non-database services found in the Docker Compose file. Domains can only be assigned to application services.</Callout>
    {/if}

    {#if !canUpdate}
      <Callout type="danger" title="Insufficient permissions">You don't have permission to manage domains. Contact your team administrator for access.</Callout>
    {/if}

    <SettingsSection id="domains-section" title="Domains" flush={hasRows} class="scroll-mt-28">
      {#snippet actions()}
        {#if hasRows}
          <div class="relative w-full sm:w-64">
            <Reicon name="search" class="pointer-events-none absolute top-1/2 left-2.5 z-10 size-3.5 -translate-y-1/2 text-neutral-400 dark:text-fg-faint" />
            <input type="search" bind:value={search} aria-label="Search services or domains" class="input h-8! w-full pl-8! text-[13px]!" placeholder="Search services or domains" />
          </div>
        {/if}
        <div class="flex flex-wrap items-center gap-2">
          {#if canUpdate}
            <div class="relative shrink-0">
              <DnsMenu bind:this={dnsMenu} kind="application" {uuid} showCheckAll={true} onchange={load} />
            </div>
            {#if !data?.labels_are_writable && (!data?.is_compose || (data?.compose_services.length ?? 0) > 0)}
              <button
                type="button"
                class="button button-highlighted"
                onclick={() => {
                  addOpen = true;
                  addError = '';
                  forces = {};
                }}
              >
                <Reicon name="plus" class="size-3.5" />
                Add domain
              </button>
            {/if}
          {/if}
        </div>
      {/snippet}

      {#if hasRows}
        <div class="data-table-header service-domains-overview-grid">
          <span>Domain</span>
          <span>Protocol redirect</span>
          <span>Domain redirect</span>
          <span>Internal port</span>
          <span>Search indexing</span>
          <span>DNS status</span>
          <span class="text-right">Actions</span>
        </div>
      {/if}
      {#if data.is_compose && data.compose_services.length === 0 && !hasRows}
        <Empty size="sm" title="No services available" description="No non-database services found in the Docker Compose file." iconName="globe" />
      {:else if data.is_compose && groups.length === 0}
        <Empty size="sm" title="No domains configured" description="Add your first domain with the Add domain button above. Choose which service receives it." iconName="globe" />
      {:else if !hasRows}
        <Empty size="sm" title="No domains configured" description="Add your first domain with the Add domain button above, or generate one with the server wildcard domain." iconName="globe" />
      {:else if data.is_compose}
        <div>
          {#each groups as group (group.name)}
            <section
              id="application-compose-domain-group-{group.name.replaceAll('.', '__dot__')}"
              class="border-b border-neutral-200 last:border-b-0 dark:border-white/10"
              class:hidden={!matches(`${group.name} ${group.rows.map((r) => r.url).join(' ')}`)}
            >
              <div class="flex w-full items-center justify-between gap-3 border-b border-neutral-200 bg-neutral-50 px-4 py-3 dark:border-white/10 dark:bg-white/[0.04]">
                <span class="min-w-0 flex-1 truncate text-sm font-medium text-black dark:text-white">{group.name}</span>
              </div>
              <div class="data-table w-full">
                {#each group.rows as row (row.url)}
                  {@render domainRow(row)}
                {/each}
              </div>
            </section>
          {/each}
          {#if search.trim() && !groups.some((g) => matches(`${g.name} ${g.rows.map((r) => r.url).join(' ')}`))}
            <div class="px-4 py-8">
              <Empty size="sm" title="No domains found" description="No service or domain matches your search." iconName="search" />
            </div>
          {/if}
        </div>
      {:else}
        <div class="data-table w-full">
          {#each rows as row (row.url)}
            {@render domainRow(row)}
          {/each}
        </div>
        {#if search.trim() && !rows.some((r) => matches(r.url))}
          <div class="px-4 py-8">
            <Empty size="sm" title="No domains found" description="No domain matches your search." iconName="search" />
          </div>
        {/if}
      {/if}
    </SettingsSection>

    {#if hasRows && !data.labels_are_writable}
      <SettingsSection id="domain-redirects-section" title="Redirects" description="Applies to all domains of this application.">
        <div class="grid gap-4 lg:grid-cols-2">
          {#if hasHttps}
            <Listbox
              id="isForceHttpsEnabled"
              label="Redirect HTTP to HTTPS"
              disabled={!canUpdate}
              helper="Disable only when Cloudflare Tunnel or another proxy connects to coolify-rs over HTTP. Keep enabled when Cloudflare uses Full or Full (Strict) SSL."
              value={data.force_https}
              options={[
                { value: true, label: 'Enabled' },
                { value: false, label: 'Disabled' },
              ]}
              onchange={(v) => saveSetting({ force_https: v })}
            />
          {/if}
          <Listbox
            id="isPermanentRedirectEnabled"
            label="Redirect type"
            disabled={!canUpdate}
            helper="Applies to the HTTP to HTTPS and www redirects. Permanent (301/308) is better for SEO, but browsers cache it. Use Temporary (302/307) while you test a redirect."
            value={data.permanent_redirect}
            options={[
              { value: false, label: 'Temporary' },
              { value: true, label: 'Permanent' },
            ]}
            onchange={(v) => saveSetting({ permanent_redirect: v })}
          />
        </div>
      </SettingsSection>
    {/if}
  </div>
{/if}

{#if addOpen}
  <Modal title="Add domain" onclose={() => (addOpen = false)}>
    <form
      class="application-settings-form flex flex-col gap-4"
      onsubmit={(e) => {
        e.preventDefault();
        forces = {};
        submitAdd();
      }}
    >
      {#if data?.is_compose && data.compose_services.length > 0}
        <Listbox id="newDomainService" label="Service" required bind:value={newService} options={data.compose_services.map((s) => ({ value: s, label: s }))} />
      {/if}
      {@render domainInput('newDomainParts', newParts, addError, undefined)}
      <div class="flex flex-wrap items-center justify-between gap-2 pt-2">
        <button type="button" class="button" onclick={generate}>Generate domain</button>
        <div class="flex flex-wrap gap-2">
          <button type="submit" class="button button-highlighted">Save</button>
        </div>
      </div>
    </form>
  </Modal>
{/if}

{#if editing}
  {@const row = editing}
  <Modal title="Domain settings" onclose={() => (editing = null)}>
    <form
      class="flex flex-col gap-4"
      onsubmit={(e) => {
        e.preventDefault();
        forces = {};
        submitEdit();
      }}
    >
      {#if row.service}
        <div class="w-full">
          <div class="mb-1.5 flex h-4 w-full items-center gap-1.5">
            <span class="mb-0! flex items-center gap-1 text-sm font-medium leading-4">Service</span>
          </div>
          <input type="text" class="input" readonly value={row.service} />
        </div>
      {/if}
      {@render domainInput('editingDomainParts', editParts, editError, row.inherited_port)}
      {#if !data?.labels_are_writable && canUpdate}
        <div class="grid grid-cols-1 gap-4 border-t border-neutral-200 pt-4 sm:grid-cols-2 dark:border-white/10">
          <Listbox
            id="application-domain-indexing"
            label="Search engine indexing"
            bind:value={editIndexing}
            options={[
              { value: 'index', label: 'Indexable' },
              { value: 'noindex', label: 'Noindex' },
            ]}
          />
          <Listbox
            id="application-domain-direction"
            label="www redirect"
            helper={data?.is_compose ? 'Applies to all domains for this Compose service.' : 'Applies to all domains for this application.'}
            bind:value={editRedirect}
            options={[
              { value: 'both', label: 'No redirect' },
              { value: 'www', label: 'Redirect to www' },
              { value: 'non-www', label: 'Redirect to non-www' },
            ]}
          />
        </div>
      {/if}
      <div class="shrink-0 border-t border-neutral-200 pt-4 dark:border-white/10">
        <div class="flex flex-wrap items-center justify-between gap-2">
          <button type="button" class="button" onclick={regenerate}>Regenerate hostname</button>
          <button type="submit" class="button button-highlighted">Save</button>
        </div>
      </div>
    </form>
  </Modal>
{/if}

{#if conflicts}
  <ServiceDomainConflict
    {conflicts}
    onproceed={() => {
      conflicts = null;
      pending?.({ force_domains: true });
    }}
    onclose={() => (conflicts = null)}
  />
{/if}

{#if unknownPort !== null}
  <Modal title="Use a different port?" onclose={() => (unknownPort = null)}>
    <Callout type="warning" title="Unrecognized internal port" class="mb-4">
      Port <strong>{unknownPort}</strong> is not listed in Ports Exposes and is not used by any application domain. The proxy will still route to it, but the container may not be listening there.
    </Callout>
    <div class="mt-4 flex flex-wrap justify-end gap-2 border-t border-neutral-200 pt-4 dark:border-white/[0.08]">
      <button type="button" class="button" onclick={() => (unknownPort = null)}>Cancel</button>
      <button
        type="button"
        class="button button-error"
        onclick={() => {
          unknownPort = null;
          pending?.({ force_port: true });
        }}>Use this port anyway</button
      >
    </div>
  </Modal>
{/if}

{#if removing}
  {@const row = removing}
  <ConfirmModal
    title="Remove domain?"
    checkboxes={[{ id: 'deleteManagedDns', label: 'Also delete the DNS record created by coolify-rs, if present.' }]}
    actions={['This domain will be removed from the application.', 'Redeploy or restart may be required for proxy changes.']}
    confirmWithText={false}
    confirmWithPassword={false}
    step2ButtonText="Remove domain"
    onconfirm={async (_password: string, selected: string[]) => {
      await remove(row, selected.includes('deleteManagedDns'));
    }}
    onclose={() => (removing = null)}
  />
{/if}
