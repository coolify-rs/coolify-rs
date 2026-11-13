<script lang="ts">
  // Port of upstream livewire/security/api-tokens.blade.php (4.7.5) and components/api-disabled-callout.blade.php.
  import Reicon from '../components/Reicon.svelte';
  import Empty from '../components/Empty.svelte';
  import Helper from '../components/Helper.svelte';
  import Callout from '../components/Callout.svelte';
  import Pager from '../components/Pager.svelte';
  import Modal from '../components/Modal.svelte';
  import RowActions from '../components/RowActions.svelte';
  import RowAction from '../components/RowAction.svelte';
  import StatusBadge from '../components/StatusBadge.svelte';
  import ConfirmModal from '../components/ConfirmModal.svelte';
  import SecurityLayout from '../components/SecurityLayout.svelte';
  import SettingsSection from '../components/SettingsSection.svelte';
  import ApiTokenFields from '../components/ApiTokenFields.svelte';
  import { ApiError } from '../lib/api';
  import { securityUi, ago, dateOf, type ApiToken, type ApiTokensPage, type TokenForm } from '../lib/securityApi';
  import { notify } from '../lib/toast.svelte';
  import { link } from '../lib/router.svelte';

  document.title = 'API Tokens | coolify-rs';

  let page = $state<ApiTokensPage | null>(null);
  let creating = $state(false);
  let editing = $state<ApiToken | null>(null);
  let issuedToken = $state<string | null>(null);
  let copied = $state(false);
  let rolling = $state<ApiToken | null>(null);
  let revoking = $state<ApiToken | null>(null);
  let search = $state('');
  let pageNumber = $state(1);
  let perPage = $state(10);

  const blankForm = (teamScope: boolean): TokenForm => ({
    description: '',
    expiresInDays: '30',
    permissions: ['read'],
    scopeMode: teamScope ? 'team' : 'selected',
    selectedScopes: [],
  });
  let form = $state<TokenForm>(blankForm(true));
  let editForm = $state<TokenForm>(blankForm(true));

  async function load() {
    const d = await securityUi.apiTokens();
    if (page === null) form = blankForm(d.can_use.team_scope);
    page = d;
    // 4.7.5: the search index (`$wire.tokenSearchIndex`) changes with the tokens, so a token shows
    // up, changes or goes away in the list right after create, edit and revoke.
  }
  load();

  const expirationList = [
    { value: '7', label: '7 days' },
    { value: '30', label: '30 days' },
    { value: '60', label: '60 days' },
    { value: '90', label: '90 days' },
    { value: '365', label: '1 year' },
    { value: '', label: 'Never' },
  ];
  const editExpiry = $derived([{ value: 'keep', label: editing?.edit.keepLabel ?? 'Keep current' }, ...expirationList]);

  const body = (f: TokenForm) => ({
    description: f.description,
    expires_in_days: f.expiresInDays === '' ? null : f.expiresInDays,
    permissions: f.permissions,
    scope_mode: f.scopeMode,
    selected_scopes: f.scopeMode === 'selected' ? f.selectedScopes : [],
  });
  const failure = (err: unknown) => notify.failure(err instanceof ApiError ? err.message : String(err));

  /** `addNewToken`. */
  async function addNewToken(e: SubmitEvent) {
    e.preventDefault();
    try {
      const r = await securityUi.createApiToken(body(form));
      form = blankForm(page?.can_use.team_scope ?? true);
      creating = false;
      issuedToken = r.token;
      await load();
    } catch (err) {
      failure(err);
    }
  }

  /** `editToken(values)`: the edit modal opens with the values of the row, without a request. */
  function editToken(t: ApiToken) {
    editing = t;
    editForm = {
      description: t.edit.description,
      expiresInDays: t.edit.expiresInDays,
      permissions: [...t.edit.permissions],
      scopeMode: t.edit.scopeMode,
      selectedScopes: [...t.edit.selectedScopes],
    };
  }

  /** `updateToken`. */
  async function updateToken(e: SubmitEvent) {
    e.preventDefault();
    if (!editing) return;
    try {
      await securityUi.updateApiToken(editing.id, body(editForm));
      editing = null;
      await load();
      notify.success('API token updated.');
    } catch (err) {
      failure(err);
    }
  }

  /** `setPaused`. */
  async function setPaused(t: ApiToken, paused: boolean) {
    try {
      const r = await securityUi.setApiTokenPaused(t.id, paused);
      await load();
      notify.success(r.message);
    } catch (err) {
      failure(err);
    }
  }

  /** `roll`. */
  async function roll() {
    if (!rolling) return;
    try {
      const r = await securityUi.rollApiToken(rolling.id);
      rolling = null;
      issuedToken = r.token;
      await load();
    } catch (err) {
      failure(err);
    }
  }

  /** `revoke`. */
  async function revoke() {
    if (!revoking) return;
    try {
      await securityUi.revokeApiToken(revoking.id);
      revoking = null;
      await load();
    } catch (err) {
      failure(err);
    }
  }

  async function copyToken() {
    await navigator.clipboard?.writeText(issuedToken ?? '').catch(() => {});
    copied = true;
    setTimeout(() => (copied = false), 1500);
  }

  const labelOf = (s: { scope: string; uuid: string }) => page?.scope_labels[`${s.scope}:${s.uuid}`];
  const searchText = (t: ApiToken) =>
    [t.name, ...(t.abilities ?? []), ...(t.scopes === null ? ['All resources in the team'] : t.scopes.map(labelOf).filter((l) => !!l))]
      .join(' ')
      .toLowerCase();
  const order = ['project', 'environment', 'resource'];
  /** The scopes of a token in the access modal: by type, then by label. */
  const tokenScopes = (t: ApiToken) =>
    (t.scopes ?? [])
      .map((s) => ({ type: s.scope || 'scope', label: labelOf(s) ?? `Unavailable ${s.scope || 'scope'}` }))
      .sort((a, b) => order.indexOf(a.type) - order.indexOf(b.type) || a.label.localeCompare(b.label, undefined, { numeric: true, sensitivity: 'base' }));
  /** `countBy('type')` joined: "1 project, 2 resources". */
  const scopeSummary = (t: ApiToken) => {
    const counts = new Map<string, number>();
    for (const s of tokenScopes(t)) counts.set(s.type, (counts.get(s.type) ?? 0) + 1);
    return [...counts].map(([type, n]) => `${n} ${n === 1 ? type : `${type}s`}`).join(', ');
  };
  let accessOf = $state<ApiToken | null>(null);

  const tokens = $derived(page?.tokens ?? []);
  const filtered = $derived(
    tokens.filter((t) => {
      const q = search.trim().toLowerCase();
      return !q || searchText(t).includes(q);
    }),
  );
  const visible = $derived(filtered.slice((pageNumber - 1) * perPage, pageNumber * perPage));
  const expired = (t: ApiToken) => !!t.expires_at && new Date(`${t.expires_at}Z`).getTime() < Date.now();
  const badgeClass = (a: string) =>
    a === 'root' || a === 'delete'
      ? 'border-error/20 bg-error/10 text-error'
      : a === 'write' || a === 'write:sensitive'
        ? 'border-warning/20 bg-warning/10 text-warning'
        : a === 'deploy'
          ? 'border-coollabs/20 bg-coollabs/10 text-coollabs dark:border-brand-light/20 dark:bg-brand/15 dark:text-brand-light'
          : 'border-neutral-200 bg-neutral-100 text-neutral-600 dark:border-white/[0.08] dark:bg-white/[0.06] dark:text-fg-dim';
</script>

<div>
  <SecurityLayout>
    <div class="application-settings-form flex flex-col gap-6">
      {#if page && !page.api_enabled}
        <Callout type="warning" title="API access is turned off">
          REST API requests are rejected. Tokens still authenticate the MCP endpoint when the MCP server is enabled.
          {#if page.instance_admin}
            <div class="mt-2">
              <a use:link href="/settings/advanced" class="button">Enable API access</a>
            </div>
          {:else}
            Ask an instance admin to enable API access.
          {/if}
        </Callout>
      {/if}

      <SettingsSection title="Issued tokens" flush>
        {#snippet actions()}
          <button type="button" class="button button-highlighted" onclick={() => (creating = true)}>
            <Reicon name="plus" class="size-3.5" />
            New token
          </button>
        {/snippet}
        {#if page}
          <div>
            {#if tokens.length > 1}
              <div class="flex items-center gap-2 border-b border-neutral-200 p-3 dark:border-white/[0.08]">
                <div class="relative w-full max-w-sm">
                  <Reicon name="search" class="pointer-events-none absolute top-1/2 left-2.5 z-10 size-3.5 -translate-y-1/2 text-neutral-400 dark:text-fg-faint" />
                  <input
                    bind:value={search}
                    oninput={() => (pageNumber = 1)}
                    type="search"
                    placeholder="Search tokens"
                    aria-label="Search tokens"
                    class="h-8! w-full rounded-lg! border-neutral-200! bg-white! py-0! pr-8! pl-8! text-[12px]! shadow-none! placeholder:text-neutral-400 focus:border-accent! focus:ring-0! dark:border-white/[0.08]! dark:bg-white/[0.035]! dark:text-fg! dark:placeholder:text-fg-faint"
                  />
                  {#if search}
                    <button
                      onclick={() => ((search = ''), (pageNumber = 1))}
                      type="button"
                      class="absolute top-1/2 right-2 flex size-5 -translate-y-1/2 items-center justify-center rounded text-neutral-400 transition-colors hover:bg-neutral-100 hover:text-black dark:text-fg-faint dark:hover:bg-white/[0.07] dark:hover:text-fg"
                      aria-label="Clear search"
                    >
                      <Reicon name="x" class="size-3" />
                    </button>
                  {/if}
                </div>
                <Helper label="Search help" helper="Search by token name, ability, or the name of a project, environment, or resource that the token can use." />
              </div>
            {/if}

            {#if tokens.length === 0}
              <div class="p-4">
                <Empty title="No API tokens" description="Create a token when an external client needs access." iconName="keys" size="sm" />
              </div>
            {:else}
              {#if filtered.length > 0}
                <div class="data-table">
                  <div class="data-table-header api-tokens-table-grid">
                    <span>Description</span>
                    <span>Permissions</span>
                    <span>Last used</span>
                    <span>Created</span>
                    <span>Expires</span>
                    <span class="text-right">Actions</span>
                  </div>
                  {#each visible as token (token.id)}
                    <div class="data-table-row api-tokens-table-grid border-b border-neutral-200 last:border-b-0 dark:border-white/[0.07]">
                      <div class="min-w-0">
                        <div class="flex min-w-0 items-center gap-1.5">
                          <span class="truncate text-[12px] font-medium text-black dark:text-fg">{token.name}</span>
                          {#if token.paused}
                            <StatusBadge label="Paused" type="warning" size="sm" />
                          {/if}
                        </div>
                        <div class="truncate text-[11px] text-neutral-500 dark:text-fg-dim">
                          {#if token.scopes === null}
                            All resources in the team
                          {:else}
                            <button type="button" class="max-w-full truncate underline decoration-dotted underline-offset-2 hover:text-black dark:hover:text-fg" onclick={() => (accessOf = token)}>
                              {scopeSummary(token)}
                            </button>
                          {/if}
                        </div>
                      </div>
                      <div class="flex min-w-0 flex-wrap items-center gap-1.5">
                        {#each token.abilities ?? [] as ability (ability)}
                          <span class="inline-flex min-h-5 items-center rounded-md border px-2 py-0.5 text-[10px] font-medium {badgeClass(ability)}">{ability}</span>
                        {/each}
                      </div>
                      <div class="text-[11px] text-neutral-500 dark:text-fg-dim">{token.last_used_at ? ago(token.last_used_at) : 'Never'}</div>
                      <div class="text-[11px] text-neutral-500 dark:text-fg-dim">{dateOf(token.created_at)}</div>
                      <div class="text-[11px] text-neutral-500 dark:text-fg-dim">
                        {#if !token.expires_at}
                          Never
                        {:else if expired(token)}
                          <StatusBadge label="Expired" type="error" size="sm" />
                        {:else}
                          {dateOf(token.expires_at)}
                        {/if}
                      </div>
                      <div class="flex items-center justify-end gap-1">
                        {#if token.own}
                          <RowActions label="Token actions">
                            <RowAction onclick={() => editToken(token)}>Edit</RowAction>
                            <RowAction onclick={() => setPaused(token, !token.paused)}>{token.paused ? 'Resume' : 'Pause'}</RowAction>
                            <RowAction onclick={() => (rolling = token)}>Roll</RowAction>
                            {#snippet danger()}
                              <RowAction onclick={() => (revoking = token)}>Revoke</RowAction>
                            {/snippet}
                          </RowActions>
                        {/if}
                      </div>
                    </div>
                  {/each}
                </div>
                <Pager total={filtered.length} bind:page={pageNumber} bind:pageSize={perPage} storageKey="coolify.page-size.api-tokens" />
              {:else}
                <div class="p-4">
                  <Empty size="sm" title="No matching tokens" description="Try a different description or permission." />
                </div>
              {/if}
            {/if}
          </div>
        {/if}
      </SettingsSection>
    </div>
  </SecurityLayout>
</div>

{#if creating && page}
  <Modal title="New API token" onclose={() => (creating = false)} widthClass="lg:w-auto lg:min-w-2xl lg:max-w-4xl">
    <form onsubmit={addNewToken} id="api-token-create-form" class="lg:w-3xl">
      <ApiTokenFields id="form" bind:form expiryOptions={expirationList} scopeOptions={page.scope_options} scopeLabels={page.scope_labels} canUse={page.can_use} />
    </form>
    {#snippet footer()}
      <button type="button" class="button" onclick={() => (creating = false)}>Cancel</button>
      <button type="submit" form="api-token-create-form" class="button button-highlighted">Create token</button>
    {/snippet}
  </Modal>
{/if}

{#if editing && page}
  <Modal title="Edit API token" onclose={() => (editing = null)} widthClass="lg:w-auto lg:min-w-2xl lg:max-w-4xl">
    <form onsubmit={updateToken} id="api-token-edit-form" class="lg:w-3xl">
      <p class="mb-4 text-[12px] text-neutral-500 dark:text-fg-dim">The token value does not change. Clients that use it get the new access on their next request.</p>
      <ApiTokenFields id="editForm" bind:form={editForm} expiryOptions={editExpiry} scopeOptions={page.scope_options} scopeLabels={page.scope_labels} canUse={page.can_use} />
    </form>
    {#snippet footer()}
      <button type="button" class="button" onclick={() => (editing = null)}>Cancel</button>
      <button type="submit" form="api-token-edit-form" class="button button-highlighted">Save changes</button>
    {/snippet}
  </Modal>
{/if}

{#if accessOf}
  <Modal title="Access of {accessOf.name}" subtitle="This token can use only these projects, environments, and resources." onclose={() => (accessOf = null)}>
    <div class="divide-y divide-neutral-200 rounded-md border border-neutral-200 dark:divide-white/[0.06] dark:border-white/[0.08]">
      {#each tokenScopes(accessOf) as s, i (i)}
        <div class="flex min-h-8 items-center gap-3 px-3 py-1.5 text-[12px]">
          <span class="w-20 shrink-0 text-[11px] capitalize text-neutral-500 dark:text-fg-dim">{s.type}</span>
          <span class="min-w-0 flex-1 break-words text-black dark:text-fg">{s.label}</span>
        </div>
      {/each}
    </div>
  </Modal>
{/if}

{#if issuedToken}
  <Modal title="Copy your token" onclose={() => (issuedToken = null)}>
    <div class="flex flex-col gap-3 lg:w-2xl">
      <p class="text-sm text-neutral-500 dark:text-fg-dim">Copy the token now. It is not shown again after you close this window.</p>
      <div class="relative">
        <input type="text" value={issuedToken} readonly onfocus={(e) => (e.currentTarget as HTMLInputElement).select()} class="input w-full pr-12! font-mono text-[12px] text-black dark:text-fg" />
        <button type="button" title="Copy token" aria-label="Copy token" class="icon-button group shrink-0 absolute top-1/2 right-2 -translate-y-1/2" onclick={copyToken}>
          <span class="inline-flex transition-transform duration-150 ease-out group-active:scale-75">
            {#if copied}<Reicon name="check" class="size-3.5 text-success" />{:else}<Reicon name="copy" class="size-3.5" />{/if}
          </span>
        </button>
      </div>
    </div>
    {#snippet footer()}
      <button type="button" class="button button-highlighted" onclick={() => (issuedToken = null)}>Done</button>
    {/snippet}
  </Modal>
{/if}

{#if rolling}
  <ConfirmModal
    title="Roll API Token?"
    actions={['A new secret replaces this token. The current secret stops working at once.', 'The description, permissions, scopes, and expiry do not change.']}
    confirmationText={rolling.name}
    confirmationLabel="Enter the token description to confirm"
    shortConfirmationLabel="Token description"
    step2ButtonText="Roll token"
    onconfirm={roll}
    onclose={() => (rolling = null)}
  />
{/if}

{#if revoking}
  <ConfirmModal
    title="Confirm API Token Revocation?"
    actions={['This API token will be permanently revoked.']}
    confirmationText={revoking.name}
    confirmationLabel="Enter the token description to confirm"
    shortConfirmationLabel="Token description"
    step2ButtonText="Revoke token"
    onconfirm={revoke}
    onclose={() => (revoking = null)}
  />
{/if}
