<script lang="ts">
  // Port of upstream livewire/project/application/preview-domains.blade.php
  // (Project\Application\PreviewDomains). DNS checks are not ported, as on the domains page.
  import Callout from '../Callout.svelte';
  import ConfirmModal from '../ConfirmModal.svelte';
  import Empty from '../Empty.svelte';
  import Listbox from '../Listbox.svelte';
  import Modal from '../Modal.svelte';
  import Reicon from '../Reicon.svelte';
  import StatusBadge from '../StatusBadge.svelte';
  import { ApiError } from '../../lib/api';
  import { previewsUi, type DomainAnswer, type DomainParts, type PreviewDomainRow, type PreviewRow, type PreviewsPage } from '../../lib/previewsApi';
  import { notify } from '../../lib/toast.svelte';

  let { uuid, page, preview, onchange }: { uuid: string; page: PreviewsPage; preview: PreviewRow; onchange: () => Promise<void> } = $props();

  const empty = (): DomainParts => ({ scheme: 'https', host: '', port: '', path: '' });

  let search = $state('');
  let addOpen = $state(false);
  let newParts = $state<DomainParts>(empty());
  let newService = $state(page.compose_services[0] ?? '');
  let addError = $state('');
  let serviceError = $state('');
  let editing = $state<PreviewDomainRow | null>(null);
  let editParts = $state<DomainParts>(empty());
  let editError = $state('');
  let removing = $state<PreviewDomainRow | null>(null);
  let unknownPort = $state<number | null>(null);
  let pending: ((force: boolean) => Promise<void>) | null = null;

  const rows = $derived(preview.rows);
  const canUpdate = $derived(page.can_update);
  const matches = (value: string) => !search.trim() || value.toLowerCase().includes(search.trim().toLowerCase());
  const groups = $derived.by(() => {
    const names: string[] = [];
    for (const r of rows) if (!names.includes(r.service ?? '')) names.push(r.service ?? '');
    return names.map((name) => ({ name, rows: rows.filter((r) => (r.service ?? '') === name) }));
  });

  const dnsType = (s: string) => (s === 'ok' ? 'success' : s === 'failed' ? 'error' : s === 'skipped' ? 'warning' : 'neutral');
  const dnsLabel = (s: string) =>
    s === 'ok' ? 'DNS matches' : s === 'failed' ? 'DNS mismatch' : s === 'skipped' ? 'DNS skipped' : s === 'checking' ? 'Checking DNS...' : s === 'pending' ? 'Not checked' : 'DNS unknown';
  const dnsUnavailable = () => notify.info('DNS checks are not available in coolify-rs yet.');
  const message = (e: unknown) => (e instanceof ApiError ? e.message : String(e));

  async function handle(call: () => Promise<DomainAnswer>, done: () => void, setError: (m: string) => void, retry: (force: boolean) => Promise<void>) {
    setError('');
    serviceError = '';
    try {
      const r = await call();
      if (r.port_warning) {
        unknownPort = r.port_warning;
        pending = retry;
        return;
      }
      if (r.message) notify.success(r.message);
      done();
      await onchange();
    } catch (e) {
      if (e instanceof ApiError && e.errors.domain?.length) setError(e.errors.domain[0]);
      else if (e instanceof ApiError && e.errors.service?.length) serviceError = e.errors.service[0];
      else notify.failure(message(e));
    }
  }

  function submitAdd(force = false): Promise<void> {
    return handle(
      () => previewsUi.domainAdd(uuid, preview.pull_request_id, { ...newParts, service: page.is_compose ? newService : null, force_port: force }),
      () => {
        newParts = empty();
        newService = page.compose_services[0] ?? '';
        addOpen = false;
      },
      (m) => (addError = m),
      submitAdd,
    );
  }

  function submitEdit(force = false): Promise<void> {
    if (!editing) return Promise.resolve();
    const row = editing;
    return handle(
      () => previewsUi.domainUpdate(uuid, preview.pull_request_id, { ...editParts, old_url: row.url, service: row.service, force_port: force }),
      () => (editing = null),
      (m) => (editError = m),
      submitEdit,
    );
  }

  async function generate() {
    try {
      notify.success((await previewsUi.domainGenerate(uuid, preview.pull_request_id, page.is_compose ? newService : null)).message);
      await onchange();
    } catch (e) {
      notify.failure(message(e));
    }
  }

  async function regenerate() {
    try {
      const { host } = await previewsUi.regenerateHost(uuid);
      editParts = { ...editParts, host: editParts.host.toLowerCase().startsWith('www.') ? `www.${host}` : host };
    } catch (e) {
      notify.failure(message(e));
    }
  }

  function openEdit(row: PreviewDomainRow) {
    editing = row;
    editParts = { ...row.editing_parts };
    editError = '';
  }

  async function remove(row: PreviewDomainRow) {
    try {
      const r = await previewsUi.domainRemove(uuid, preview.pull_request_id, row.key);
      if (r.message) notify.success(r.message);
    } catch (e) {
      notify.failure(message(e));
    }
    await onchange();
  }
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

<div class="domains-overview-container flex flex-col gap-3">
  <div class="flex flex-wrap items-center gap-2">
    <p class="min-w-0 flex-1 truncate text-[13px] text-neutral-500 dark:text-fg-dim">{rows.length} domain{rows.length === 1 ? '' : 's'}</p>
    {#if rows.length > 0}
      <input type="search" bind:value={search} aria-label="Search preview domains" class="input h-8! w-full sm:w-64!" placeholder="Search services or domains" />
    {/if}
    {#if canUpdate}
      {#if rows.length > 0}
        <button type="button" class="button" onclick={dnsUnavailable}>
          <Reicon name="refresh" class="size-3.5" />
          Check all DNS
        </button>
      {/if}
      <button
        type="button"
        class="button button-highlighted"
        onclick={() => {
          addOpen = true;
          addError = '';
        }}
      >
        <Reicon name="plus" class="size-3.5" />
        Add domain
      </button>
    {/if}
  </div>

  {#if rows.length === 0}
    <div class="application-settings-section-body">
      <Empty size="sm" title="No domains configured" description="Add a domain or generate one with the server wildcard domain." iconName="globe" />
    </div>
  {:else}
    <div class="application-settings-section-body is-flush overflow-visible">
      <div class="data-table-header service-domains-overview-grid">
        <span>Domain</span>
        <span>Protocol redirect</span>
        <span>Domain redirect</span>
        <span>Internal port</span>
        <span>Search indexing</span>
        <span>DNS status</span>
        <span class="text-right">Actions</span>
      </div>
      {#each groups as group (group.name)}
        {#if page.is_compose}
          <div
            class:hidden={!matches(`${group.name} ${group.rows.map((r) => r.url).join(' ')}`)}
            class="border-b border-neutral-200 bg-neutral-50 px-4 py-3 text-sm font-medium dark:border-white/10 dark:bg-white/[0.04]"
          >
            {group.name}
          </div>
        {/if}
        {#each group.rows as row (row.key)}
          {@const forced = row.url.startsWith('https://') && page.force_https}
          <div class="env-table-item" class:hidden={!matches(`${row.service ?? ''} ${row.url}`)}>
            <div class="data-table-row service-domains-overview-grid">
              <div class="flex min-w-0 flex-col gap-1">
                <div class="flex min-w-0 items-center gap-2">
                  <Reicon name="globe" class="size-4 shrink-0 text-neutral-400 dark:text-fg-faint" />
                  <a
                    href={row.public_url}
                    target="_blank"
                    rel="noopener noreferrer"
                    class="min-w-0 flex-1 truncate text-[13px] text-black underline decoration-neutral-300 underline-offset-2 hover:decoration-coollabs sm:truncate dark:text-fg dark:decoration-white/20"
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
                <span role="img" aria-label="Search indexing blocked" title="Search indexing blocked">
                  <Reicon name="x" class="size-4" />
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
                <span>Noindex</span>
              </div>
              <div class="service-domain-dns flex min-w-0 items-center">
                <span title={row.dns_message}><StatusBadge label={dnsLabel(row.dns_status)} type={dnsType(row.dns_status)} /></span>
              </div>
              <div class="service-domain-actions flex items-center justify-end gap-1">
                {#if canUpdate}
                  <button type="button" class="icon-button shrink-0" title="Check DNS" aria-label="Check DNS" onclick={dnsUnavailable}>
                    <Reicon name="refresh" class="size-3.5" />
                  </button>
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
              </div>
            </div>
          </div>
        {/each}
      {/each}
      {#if search.trim() && !rows.some((r) => matches(`${r.service ?? ''} ${r.url}`))}
        <div class="px-4 py-8">
          <Empty size="sm" title="No domains found" description="No service or domain matches your search." iconName="search" />
        </div>
      {/if}
    </div>
  {/if}
</div>

{#if addOpen}
  <Modal title="Add domain" onclose={() => (addOpen = false)}>
    <form
      class="application-settings-form flex flex-col gap-4"
      onsubmit={(e) => {
        e.preventDefault();
        submitAdd();
      }}
    >
      {#if page.is_compose && page.compose_services.length > 0}
        <Listbox id="newDomainService" label="Service" required bind:value={newService} options={page.compose_services.map((s) => ({ value: s, label: s }))} />
        {#if serviceError}<p class="-mt-3 text-[12px] text-red-500">{serviceError}</p>{/if}
      {/if}
      {@render domainInput(`newDomainParts-${preview.id}`, newParts, addError)}
      <div class="flex flex-wrap items-center justify-between gap-2 pt-2">
        <button type="button" class="button" onclick={generate}>Generate domain</button>
        <button type="submit" class="button button-highlighted">Save</button>
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
        submitEdit();
      }}
    >
      {#if row.service}
        <div>
          <div class="mb-1.5 flex h-4 items-center">
            <!-- svelte-ignore a11y_label_has_associated_control -->
            <label class="mb-0! leading-4">Service</label>
          </div>
          <input type="text" class="input" readonly value={row.service} />
        </div>
      {/if}
      {@render domainInput(`editingDomainParts-${preview.id}`, editParts, editError)}
      <div class="grid grid-cols-1 gap-4 border-t border-neutral-200 pt-4 sm:grid-cols-2 dark:border-white/10">
        <Listbox
          id="preview-domain-indexing-{preview.id}"
          label="Search engine indexing"
          value="noindex"
          disabled
          helper="Preview deployments are always excluded from search indexing."
          options={[{ value: 'noindex', label: 'Noindex' }]}
        />
        <Listbox
          id="preview-domain-direction-{preview.id}"
          label="www redirect"
          value={row.redirect}
          disabled
          helper="Read-only preview routing configuration."
          options={[
            { value: 'both', label: 'No redirect' },
            { value: 'www', label: 'Redirect to www' },
            { value: 'non-www', label: 'Redirect to non-www' },
          ]}
        />
      </div>
      <div class="flex flex-wrap items-center justify-between gap-2 border-t border-neutral-200 pt-4 dark:border-white/10">
        <button type="button" class="button" onclick={regenerate}>Regenerate hostname</button>
        <button type="submit" class="button button-highlighted">Save</button>
      </div>
    </form>
  </Modal>
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
          pending?.(true);
        }}>Use this port anyway</button
      >
    </div>
  </Modal>
{/if}

{#if removing}
  {@const row = removing}
  <ConfirmModal
    title="Remove domain?"
    actions={['This domain will be removed from the preview deployment.', 'Redeploy the preview to apply proxy changes.']}
    confirmWithText={false}
    confirmWithPassword={false}
    step2ButtonText="Remove domain"
    onconfirm={async () => {
      await remove(row);
    }}
    onclose={() => (removing = null)}
  />
{/if}
