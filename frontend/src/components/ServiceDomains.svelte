<script lang="ts">
  // Port of upstream livewire/project/service/domains.blade.php and
  // partials/domain-table.blade.php (Project\Service\Domains). DNS checks and the
  // Cloudflare automation are not ported: they contact external services.
  import Callout from './Callout.svelte';
  import ConfirmModal from './ConfirmModal.svelte';
  import Empty from './Empty.svelte';
  import Helper from './Helper.svelte';
  import Listbox from './Listbox.svelte';
  import Modal from './Modal.svelte';
  import Reicon from './Reicon.svelte';
  import RowActions from './RowActions.svelte';
  import RowAction from './RowAction.svelte';
  import DnsMenu from './shared/DnsMenu.svelte';
  import { followDnsJobs, hostOf, type AfterAddDns } from '../lib/dnsAfterAdd';
  import { request } from '../lib/api';
  import { resPath } from '../lib/sharedApi';
  import ServiceDomainConflict from './ServiceDomainConflict.svelte';
  import StatusBadge from './StatusBadge.svelte';
  import { ApiError } from '../lib/api';
  import { headline, servicesApi, type DomainParts, type ServiceDomainApp, type ServiceDomainRow, type ServiceDomains } from '../lib/servicesApi';
  import { notify } from '../lib/toast.svelte';

  let { serviceUuid }: { serviceUuid: string } = $props();

  let data = $state<ServiceDomains | null>(null);
  let search = $state('');

  const empty = (): DomainParts => ({ scheme: 'https', host: '', port: '', path: '' });

  let addOpen = $state(false);
  let newApp = $state<number | null>(null);
  let newParts = $state<DomainParts>(empty());
  let addError = $state('');

  let editing = $state<ServiceDomainRow | null>(null);
  let editParts = $state<DomainParts>(empty());
  let editIndexing = $state<'index' | 'noindex'>('index');
  let editRedirect = $state('both');
  let editError = $state('');

  let removing = $state<ServiceDomainRow | null>(null);
  let conflicts = $state<{ domain: string; resource_name: string; resource_link?: string | null; resource_type: string }[] | null>(null);
  let requiredPort = $state<number | null>(null);
  let pending: ((force: { force_domains?: boolean; force_port?: boolean }) => Promise<void>) | null = null;
  let forces: { force_domains?: boolean; force_port?: boolean } = {};

  const message = (e: unknown) => (e instanceof ApiError ? e.message : String(e));

  async function load() {
    data = await servicesApi.domains(serviceUuid);
    if (newApp === null && data.apps.length > 0) newApp = data.apps[0].id;
  }

  $effect(() => {
    load().catch((e) => notify.error(message(e)));
  });

  const apps = $derived(data?.apps ?? []);
  const rows = $derived(data?.rows ?? []);
  const canUpdate = $derived(!!data?.can_update);
  const groups = $derived(
    apps
      .map((app) => ({ app, rows: rows.filter((r) => r.service_application_id === app.id), heading: headline(app.name) }))
      .filter((g) => g.rows.length > 0),
  );
  const matches = (value: string) => !search.trim() || value.toLowerCase().includes(search.trim().toLowerCase());
  const shown = $derived(groups.filter((g) => matches(`${g.heading} ${g.rows.map((r) => r.url).join(' ')}`)));

  let dnsMenu = $state<DnsMenu>();
  // `markDnsPending`: on the page only, a pending status is not stored.
  let dnsPending = $state<{ hosts: string[]; message: string }>({ hosts: [], message: '' });
  const dnsTitle = (row: { url: string; dns_status: string; dns_message: string }) =>
    row.dns_status === 'ok' ? undefined : row.dns_status === 'pending' && dnsPending.hosts.includes(hostOf(row.url)) ? dnsPending.message : row.dns_message;
  const dnsType = (s: string) => (s === 'ok' ? 'success' : s === 'failed' ? 'error' : s === 'skipped' ? 'warning' : 'neutral');
  const dnsLabel = (s: string) =>
    s === 'ok' ? 'DNS matches' : s === 'failed' ? 'DNS mismatch' : s === 'skipped' ? 'DNS skipped' : s === 'checking' ? 'Checking DNS...' : s === 'pending' ? 'Not checked' : 'DNS unknown';

  async function handle(
    call: () => ReturnType<typeof servicesApi.addDomain>,
    done: () => void,
    setError: (m: string) => void,
    retry: (force: { force_domains?: boolean; force_port?: boolean }) => Promise<void>,
  ) {
    setError('');
    try {
      const r = await call();
      if (r.domain_conflicts) {
        conflicts = r.domain_conflicts;
        pending = retry;
        return;
      }
      if (r.port_warning) {
        requiredPort = r.port_warning;
        pending = retry;
        return;
      }
      forces = {};
      for (const w of r.warnings ?? []) notify.warning(w);
      const dns = (r as { dns?: AfterAddDns }).dns;
      for (const i of dns?.info ?? []) notify.info(i);
      if (r.message) notify.success(r.message);
      done();
      await load();
      if (dns) {
        dnsPending = { hosts: dns.pending ?? [], message: dns.pending_message ?? '' };
        if (dns.open_modal) void dnsMenu?.openProviders();
        if (dns.jobs?.length) followDnsJobs('service', serviceUuid, dns.jobs, load);
      }
    } catch (e) {
      if (e instanceof ApiError && e.errors.domain?.length) setError(e.errors.domain[0]);
      else notify.error(message(e));
    }
  }

  function submitAdd(force: { force_domains?: boolean; force_port?: boolean } = {}): Promise<void> {
    forces = { ...forces, ...force };
    return handle(
      () => servicesApi.addDomain(serviceUuid, { app_id: newApp, ...newParts, ...forces }),
      () => {
        addOpen = false;
        newParts = empty();
      },
      (m) => (addError = m),
      submitAdd,
    );
  }

  function submitEdit(force: { force_domains?: boolean; force_port?: boolean } = {}): Promise<void> {
    if (!editing) return Promise.resolve();
    const row = editing;
    forces = { ...forces, ...force };
    // `updateDomain`: an inherited port shown in the field stays inherited.
    const parts = { ...editParts };
    if (!row.has_port_override && String(parts.port ?? '') === String(row.internal_port ?? '')) parts.port = '';
    return handle(
      () =>
        servicesApi.updateDomain(serviceUuid, {
          app_id: row.service_application_id,
          old_url: row.url,
          ...parts,
          indexing: editIndexing,
          redirect: editRedirect,
          ...forces,
        }),
      () => (editing = null),
      (m) => (editError = m),
      submitEdit,
    );
  }

  function openEdit(row: ServiceDomainRow, app: ServiceDomainApp) {
    editing = row;
    editParts = { ...row.editing_parts };
    editIndexing = row.is_noindexed ? 'noindex' : 'index';
    editRedirect = app.redirect;
    editError = '';
  }

  async function generate() {
    try {
      newParts = (await servicesApi.generateDomain(serviceUuid, newApp)).parts;
    } catch (e) {
      notify.error(message(e));
    }
  }

  async function regenerate() {
    try {
      const { parts } = await servicesApi.generateDomain(serviceUuid);
      editParts = { ...editParts, host: editParts.host.toLowerCase().startsWith('www.') ? `www.${parts.host}` : parts.host };
    } catch (e) {
      notify.error(message(e));
    }
  }

  async function saveSetting(app: ServiceDomainApp, body: { force_https?: boolean; permanent_redirect?: boolean }) {
    try {
      notify.success((await servicesApi.domainSettings(serviceUuid, { app_id: app.id, ...body })).message);
    } catch (e) {
      notify.error(message(e));
    }
    await load();
  }

  async function remove(row: ServiceDomainRow, deleteManagedDns = false) {
    try {
      const r = await servicesApi.removeDomain(serviceUuid, { app_id: row.service_application_id, url: row.url, delete_managed_dns: deleteManagedDns });
      for (const w of r.warnings ?? []) notify.warning(w);
      if (r.message) notify.success(r.message);
    } catch (e) {
      notify.error(message(e));
    }
    await load();
  }

  async function checkDns(row: ServiceDomainRow) {
    try {
      await request('POST', `${resPath('service', serviceUuid)}/dns/check`, { url: row.url });
      await load();
    } catch (e) {
      notify.error(message(e));
    }
  }
  // `pollDnsChecks`: while a check runs, and its toast when it ends.
  let checkingNow = new Set<string>();
  $effect(() => {
    if (!rows.some((r) => r.dns_status === 'checking')) return;
    const timer = setTimeout(load, 2000);
    return () => clearTimeout(timer);
  });
  $effect(() => {
    const now = new Set<string>();
    for (const r of rows) {
      if (r.dns_status === 'checking') now.add(r.url);
      else if (checkingNow.has(r.url)) {
        let host = r.url;
        try { host = new URL(r.url).hostname; } catch { /* keep the URL */ }
        if (r.dns_status === 'ok') notify.success(`DNS is configured correctly for ${host}.`);
        else if (r.dns_status === 'failed') notify.error(`DNS is not configured for ${host}. Review the required DNS record. If you changed it recently, DNS propagation can take some time, so please try again later.`);
        else notify.info(`DNS check skipped for ${host}.`);
      }
    }
    checkingNow = now;
  });
</script>

{#snippet domainInput(prefix: string, parts: DomainParts, error: string)}
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
      <input id="{prefix}-port" type="number" class="input" bind:value={parts.port} placeholder="3000" min="1" max="65535" inputmode="numeric" />
    </div>
    <div class="min-w-0 sm:col-span-3">
      <div class="mb-1.5 flex h-4 w-full items-center gap-1.5">
        <label for="{prefix}-path" class="mb-0! flex items-center gap-1.5 leading-4">Path</label>
      </div>
      <input id="{prefix}-path" type="text" class="input" bind:value={parts.path} placeholder="/api/v3" autocomplete="off" />
      <p class="mt-1 text-[12px] text-neutral-500 dark:text-fg-dim">Optional path, query, or fragment appended after the domain and port.</p>
    </div>
  </div>
{/snippet}

{#if data}
  <div id="service-domains-section" class="flex flex-col gap-4">
    {#if !canUpdate}
      <Callout type="danger" title="Insufficient permissions">You don't have permission to manage domains. Contact your team administrator for access.</Callout>
    {/if}

    <div id="domains-section" class="flex flex-wrap items-center gap-2">
      {#if groups.length > 0}
        <div class="relative w-full sm:w-64">
          <Reicon name="search" class="pointer-events-none absolute top-1/2 left-2.5 z-10 size-3.5 -translate-y-1/2 text-neutral-400 dark:text-fg-faint" />
          <input type="search" bind:value={search} aria-label="Search services or domains" class="input h-8! w-full pl-8! text-[13px]!" placeholder="Search services or domains" />
        </div>
      {/if}
      <div class="ml-auto flex flex-wrap items-center gap-2">
        {#if canUpdate && apps.length > 0}
          <DnsMenu bind:this={dnsMenu} kind="service" uuid={serviceUuid} showCheckAll={true} onchange={load} />
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
      </div>
    </div>

    {#if apps.length === 0}
      <div class="application-settings-section-body mt-1 w-full scroll-mt-28">
        <Empty size="sm" title="No application services" description="Only database services are available. Domains can only be assigned to application services." iconName="globe" />
      </div>
    {:else if rows.length === 0}
      <div class="application-settings-section-body mt-1 w-full scroll-mt-28">
        <Empty size="sm" title="No domains configured" description="Add your first domain with the Add domain button above. Choose which service application receives it." iconName="globe" />
      </div>
    {:else}
      <div class="flex flex-col gap-3">
        {#each shown as group (group.app.id)}
          {@const hasHttps = group.rows.some((r) => r.url.toLowerCase().startsWith('https://'))}
          <section id="service-domain-group-{group.app.id}" class="application-settings-section-body is-flush overflow-visible">
            <div class="flex w-full flex-wrap items-center gap-3 rounded-t-lg border-b border-neutral-200 bg-neutral-50 px-4 py-3 dark:border-white/10 dark:bg-white/[0.03]">
              <span class="min-w-0 flex-1 truncate text-sm font-medium text-black dark:text-white">{group.heading}</span>
              {#if hasHttps}
                <div class="flex w-full items-center gap-2 sm:w-auto service-domains-https">
                  <span class="mb-0! whitespace-nowrap text-[12px]!">Redirect HTTP to HTTPS</span>
                  <Helper helper="Disable only when Cloudflare Tunnel or another proxy connects to coolify-rs over HTTP. Keep enabled when Cloudflare uses Full or Full (strict) SSL." />
                  <Listbox
                    id="service-force-https-{group.app.id}"
                    disabled={!canUpdate}
                    value={group.app.force_https}
                    options={[
                      { value: true, label: 'Enabled' },
                      { value: false, label: 'Disabled' },
                    ]}
                    onchange={(v) => saveSetting(group.app, { force_https: v })}
                  />
                </div>
              {/if}
              <div class="flex w-full items-center gap-2 sm:w-auto service-domains-https">
                <span class="mb-0! whitespace-nowrap text-[12px]!">Redirect type</span>
                <Helper helper="Applies to the HTTP to HTTPS and www redirects. Permanent (301/308) is better for SEO, but browsers cache it. Use Temporary while testing." />
                <Listbox
                  id="service-permanent-redirect-{group.app.id}"
                  disabled={!canUpdate}
                  value={group.app.permanent_redirect}
                  options={[
                    { value: false, label: 'Temporary' },
                    { value: true, label: 'Permanent' },
                  ]}
                  onchange={(v) => saveSetting(group.app, { permanent_redirect: v })}
                />
              </div>
            </div>

            <div class="data-table w-full">
              <div class="data-table-header service-domains-overview-grid">
                <span>Domain</span>
                <span>Protocol redirect</span>
                <span>Domain redirect</span>
                <span>Internal port</span>
                <span>Search indexing</span>
                <span>DNS status</span>
                <span class="text-right">Actions</span>
              </div>
              {#each group.rows as row (row.url)}
                {@const forced = row.url.startsWith('https://') && group.app.force_https}
                <div class="env-table-item">
                  <div class="data-table-row service-domains-overview-grid">
                    <div class="flex min-w-0 flex-col gap-1">
                      <div class="flex min-w-0 items-center gap-2">
                        <a
                          href={row.public_url}
                          target="_blank"
                          rel="noopener noreferrer"
                          class="min-w-0 flex-1 truncate text-[13px] text-black underline decoration-neutral-300 underline-offset-2 hover:decoration-coollabs dark:text-white dark:decoration-white/30"
                          title={row.public_url}>{row.public_url}</a
                        >
                      </div>
                    </div>
                    <div class="service-domain-detail" title="Protocol redirect">
                      <span class="service-domain-detail-label">Protocol redirect</span>
                      <span>{forced ? 'HTTP → HTTPS' : 'Disabled'}</span>
                    </div>
                    <div class="service-domain-detail" title={group.app.redirect === 'www' ? 'Redirect to www' : group.app.redirect === 'non-www' ? 'Redirect to non-www' : 'Both www and non-www'}>
                      <span class="service-domain-detail-label">Domain redirect</span>
                      <span>{group.app.redirect === 'www' ? 'non-www → www' : group.app.redirect === 'non-www' ? 'www → non-www' : 'Disabled'}</span>
                    </div>
                    <div class="service-domain-detail" title={row.has_port_override ? 'Custom internal port for this domain' : 'Inherited from the coolify-rs service port'}>
                      <span class="service-domain-detail-label">Internal port</span>
                      <span>{row.internal_port ?? '—'}</span>
                    </div>
                    <div class="service-domain-detail">
                      <span class="service-domain-detail-label">Search indexing</span>
                      <span role="img" aria-label={row.is_noindexed ? 'Search indexing blocked' : 'Search indexing allowed'} title={row.is_noindexed ? 'Search indexing blocked' : 'Search indexing allowed'}>
                        <Reicon name={row.is_noindexed ? 'x' : 'check'} class="size-4" />
                      </span>
                    </div>
                    <div class="service-domain-mobile-summary" aria-label="Domain routing summary">
                      {#if forced}<span>HTTP → HTTPS</span>{/if}
                      {#if group.app.redirect === 'www' || group.app.redirect === 'non-www'}
                        <span>{group.app.redirect === 'www' ? 'non-www → www' : 'www → non-www'}</span>
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
                        <RowActions label="Domain actions">
                          <RowAction onclick={() => checkDns(row)}>Check DNS</RowAction>
                          <RowAction ariaLabel="Edit {row.public_url}" onclick={() => openEdit(row, group.app)}>Edit</RowAction>
                          {#snippet danger()}
                            <RowAction onclick={() => (removing = row)}>Delete</RowAction>
                          {/snippet}
                        </RowActions>
                      {/if}
                    </div>
                  </div>
                </div>
              {/each}
            </div>
          </section>
        {/each}
        {#if search.trim() && shown.length === 0}
          <div class="px-4 py-8">
            <Empty size="sm" title="No domains found" description="No service or domain matches your search." iconName="search" />
          </div>
        {/if}
      </div>
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
      <Listbox
        id="newServiceApplicationId"
        label="Service application"
        required
        helper="Domain will be assigned to this compose service application."
        bind:value={newApp as unknown as string}
        options={apps.map((a) => ({ value: a.id as unknown as string, label: a.name + (a.image ? ` (${a.image})` : '') }))}
      />
      {@render domainInput('newDomainParts', newParts, addError)}
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
  <Modal title="Domain settings" focusFirstField={false} onclose={() => (editing = null)}>
    <form
      class="flex flex-col gap-4"
      onsubmit={(e) => {
        e.preventDefault();
        forces = {};
        submitEdit();
      }}
    >
      <div class="w-full">
        <div class="mb-1.5 flex h-4 w-full items-center gap-1.5">
          <span class="mb-0! flex items-center gap-1 text-sm font-medium leading-4">Service application</span>
        </div>
        <input type="text" class="input" readonly value={headline(apps.find((a) => a.id === editing?.service_application_id)?.name ?? '')} />
        <p class="mt-1 text-[12px] text-neutral-500 dark:text-fg-dim">Domains stay on the service they were added to. Remove and re-add to move.</p>
      </div>
      {@render domainInput('editingDomainParts', editParts, editError)}
    </form>
    <div class="mt-4 grid grid-cols-1 gap-4 border-t border-neutral-200 pt-4 sm:grid-cols-2 dark:border-white/10">
      <Listbox
        id="service-domain-indexing"
        label="Search engine indexing"
        bind:value={editIndexing}
        options={[
          { value: 'index', label: 'Indexable' },
          { value: 'noindex', label: 'Noindex' },
        ]}
      />
      <Listbox
        id="service-domain-direction"
        label="www redirect"
        helper="Applies to all domains for this service application."
        bind:value={editRedirect}
        options={[
          { value: 'both', label: 'No redirect' },
          { value: 'www', label: 'Redirect to www' },
          { value: 'non-www', label: 'Redirect to non-www' },
        ]}
      />
    </div>
    <div class="mt-4 flex flex-wrap items-center justify-between gap-2 border-t border-neutral-200 pt-4 dark:border-white/10">
      <button type="button" class="button" onclick={regenerate}>Regenerate hostname</button>
      <button
        type="button"
        class="button button-highlighted"
        onclick={() => {
          forces = {};
          submitEdit();
        }}>Save</button
      >
    </div>
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

{#if requiredPort !== null}
  <Modal title="Use a different port?" onclose={() => (requiredPort = null)}>
    <Callout type="warning" title="Port requirement" class="mb-4">
      This service requires port <strong>{requiredPort}</strong> to function correctly. One or more of your domains use a different port, or none.
    </Callout>
    <div class="mt-4 flex flex-wrap justify-end gap-2 border-t border-neutral-200 pt-4 dark:border-white/[0.08]">
      <button type="button" class="button" onclick={() => (requiredPort = null)}>Keep required port</button>
      <button
        type="button"
        class="button button-error"
        onclick={() => {
          requiredPort = null;
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
    actions={['This domain will be removed from the service application.', 'Redeploy or restart may be required for proxy changes.']}
    confirmWithText={false}
    confirmWithPassword={false}
    step2ButtonText="Remove domain"
    onconfirm={async (_password: string, selected: string[]) => {
      await remove(row, selected.includes('deleteManagedDns'));
    }}
    onclose={() => (removing = null)}
  />
{/if}
