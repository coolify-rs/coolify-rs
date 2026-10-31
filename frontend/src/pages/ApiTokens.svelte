<script lang="ts">
  // Port of upstream livewire/security/api-tokens.blade.php and components/api-disabled-callout.blade.php.
  import Reicon from '../components/Reicon.svelte';
  import Empty from '../components/Empty.svelte';
  import Helper from '../components/Helper.svelte';
  import Callout from '../components/Callout.svelte';
  import Pager from '../components/Pager.svelte';
  import FormInput from '../components/FormInput.svelte';
  import Listbox from '../components/Listbox.svelte';
  import Checkbox from '../components/Checkbox.svelte';
  import StatusBadge from '../components/StatusBadge.svelte';
  import ConfirmModal from '../components/ConfirmModal.svelte';
  import SecurityLayout from '../components/SecurityLayout.svelte';
  import SettingsSection from '../components/SettingsSection.svelte';
  import { ApiError } from '../lib/api';
  import { securityUi, ago, dateOf, type ApiToken } from '../lib/securityApi';
  import { notify } from '../lib/toast.svelte';
  import { link } from '../lib/router.svelte';

  document.title = 'API Tokens | coolify-rs';

  let tokens = $state<ApiToken[] | null>(null);
  let listed = $state<number[] | null>(null);
  let elevated = $state(false);
  let apiEnabled = $state(true);
  let instanceAdmin = $state(false);
  let description = $state('');
  let expires = $state('30');
  let permissions = $state<string[]>(['read']);
  let permissionsOpen = $state(false);
  let permissionRoot: HTMLDivElement | undefined = $state();
  let newToken = $state<string | null>(null);
  let copied = $state(false);
  let revoking = $state<ApiToken | null>(null);
  let search = $state('');
  let page = $state(1);
  let perPage = $state(10);

  async function load() {
    const d = await securityUi.apiTokens();
    tokens = d.tokens;
    // Upstream's Alpine list is rendered once: tokens created later stay hidden until a reload.
    listed ??= d.tokens.map((t) => t.id);
    elevated = d.elevated;
    apiEnabled = d.api_enabled;
    instanceAdmin = d.instance_admin;
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

  const headline = (p: string) =>
    p
      .replace(':', ' ')
      .split(' ')
      .map((w) => w.charAt(0).toUpperCase() + w.slice(1))
      .join(' ');

  /** `updatedPermissions`: Livewire calls it for every array index that changed. */
  function toggle(permission: string, checked: boolean) {
    const old = permissions;
    const changed = checked ? [...old, permission] : old.filter((p) => p !== permission);
    let next = [...changed];
    for (let i = 0; i < changed.length; i++) {
      if (changed[i] === old[i]) continue;
      if (changed[i] === 'root' && next.includes('root')) next = ['root'];
      else if (changed[i] === 'read:sensitive' && !next.includes('read')) next.push('read');
      next.sort();
    }
    if (next.length === 0) next = ['read'];
    permissions = next.sort();
  }

  async function addNewToken(e: SubmitEvent) {
    e.preventDefault();
    try {
      const r = await securityUi.createApiToken({ description, expires_in_days: expires === '' ? null : expires, permissions });
      newToken = r.token;
      await load();
    } catch (err) {
      notify.failure(err instanceof ApiError ? err.message : String(err));
    }
  }

  async function revoke() {
    if (!revoking) return;
    try {
      await securityUi.revokeApiToken(revoking.id);
      revoking = null;
      await load();
    } catch (err) {
      notify.failure(err instanceof ApiError ? err.message : String(err));
    }
  }

  async function copyToken() {
    await navigator.clipboard?.writeText(newToken ?? '').catch(() => {});
    copied = true;
    setTimeout(() => (copied = false), 1500);
  }

  const filtered = $derived(
    (tokens ?? []).filter((t) => {
      if (!listed?.includes(t.id)) return false;
      const q = search.trim().toLowerCase();
      return !q || `${t.name} ${(t.abilities ?? []).join(' ')}`.toLowerCase().includes(q);
    }),
  );
  const visible = $derived(filtered.slice((page - 1) * perPage, page * perPage));
  const expired = (t: ApiToken) => !!t.expires_at && new Date(`${t.expires_at}Z`).getTime() < Date.now();
  const badgeClass = (a: string) =>
    a === 'root'
      ? 'border-error/20 bg-error/10 text-error'
      : a === 'write' || a === 'write:sensitive'
        ? 'border-warning/20 bg-warning/10 text-warning'
        : a === 'deploy'
          ? 'border-coollabs/20 bg-coollabs/10 text-coollabs dark:border-brand-light/20 dark:bg-brand/15 dark:text-brand-light'
          : 'border-neutral-200 bg-neutral-100 text-neutral-600 dark:border-white/[0.08] dark:bg-white/[0.06] dark:text-fg-dim';
</script>

<svelte:window
  onclick={(e) => permissionsOpen && permissionRoot && !permissionRoot.contains(e.target as Node) && (permissionsOpen = false)}
  onkeydown={(e) => e.key === 'Escape' && (permissionsOpen = false)}
/>

<div>
  <SecurityLayout>
    <div class="application-settings-form flex flex-col gap-6">
      {#if !apiEnabled}
        <Callout type="warning" title="API access is turned off">
          REST API requests are rejected. Tokens still authenticate the MCP endpoint when the MCP server is enabled.
          {#if instanceAdmin}
            <div class="mt-2">
              <a use:link href="/settings/advanced" class="button">Enable API access</a>
            </div>
          {:else}
            Ask an instance admin to enable API access.
          {/if}
        </Callout>
      {/if}

      <form onsubmit={addNewToken}>
        <SettingsSection title="New API token">
          {#snippet actions()}
            <button type="submit" class="button button-highlighted">
              <Reicon name="plus" class="size-3.5" />
              Create token
            </button>
          {/snippet}

          <div class="grid gap-4 lg:grid-cols-2">
            <FormInput required id="description" label="Description" placeholder="CI deployment token" bind:value={description} />
            <Listbox id="expiresInDays" label="Expires in" options={expirationList} bind:value={expires} />
          </div>

          <div class="mt-5 border-t border-neutral-200 pt-4 dark:border-white/[0.08]">
            <div class="mb-3 flex items-center gap-2">
              <h4 class="text-[12px] font-semibold text-black dark:text-fg">Permissions</h4>
              <Helper helper="Only grant the abilities this token needs." />
            </div>
            <div class="relative" bind:this={permissionRoot}>
              <button type="button" class="listbox-trigger" onclick={() => (permissionsOpen = !permissionsOpen)} aria-haspopup="listbox" aria-expanded={permissionsOpen}>
                <span class="truncate">
                  Selected permissions: {permissions.map(headline).join(', ')}
                </span>
                <Reicon name="chevron-down" class="size-3.5 shrink-0 opacity-60" />
              </button>

              {#if permissionsOpen}
                <div class="listbox-panel top-full! mt-1! w-full!" role="listbox">
                  <div class="listbox-option p-0!">
                    <Checkbox
                      id="permission-root"
                      label="Root"
                      fullWidth
                      helper="Full access to every API operation."
                      checked={permissions.includes('root')}
                      disabled={!elevated}
                      onchange={(c) => toggle('root', c)}
                    />
                  </div>
                  <div class="listbox-option p-0!">
                    <Checkbox
                      id="permission-write"
                      label="Write"
                      fullWidth
                      helper="Create and update resources."
                      checked={permissions.includes('write')}
                      disabled={permissions.includes('root') || !elevated}
                      onchange={(c) => toggle('write', c)}
                    />
                  </div>
                  <div class="listbox-option p-0!">
                    <Checkbox
                      id="permission-deploy"
                      label="Deploy"
                      fullWidth
                      helper="Deploy, start, stop, and restart resources. With Write, also apply changes that run on servers immediately, such as scheduled tasks, proxy configuration, and imports."
                      checked={permissions.includes('deploy')}
                      disabled={permissions.includes('root') || !elevated}
                      onchange={(c) => toggle('deploy', c)}
                    />
                  </div>
                  <div class="listbox-option p-0!">
                    <Checkbox
                      id="permission-read"
                      label="Read"
                      fullWidth
                      helper="Read non-sensitive resource data."
                      checked={permissions.includes('read')}
                      disabled={permissions.includes('root')}
                      onchange={(c) => toggle('read', c)}
                    />
                  </div>
                  <div class="listbox-option p-0!">
                    <Checkbox
                      id="permission-read-sensitive"
                      label="Read sensitive data"
                      fullWidth
                      helper="Include secrets, logs, passwords, and Compose content."
                      checked={permissions.includes('read:sensitive')}
                      disabled={permissions.includes('root') || !elevated}
                      onchange={(c) => toggle('read:sensitive', c)}
                    />
                  </div>
                </div>
              {/if}
            </div>
          </div>
        </SettingsSection>
      </form>

      {#if newToken}
        <SettingsSection title="Copy your token">
          <div class="flex flex-col gap-3">
            <p class="text-sm text-neutral-500 dark:text-fg-dim">This value will not be shown again after you leave this page.</p>
            <div class="relative">
              <input type="text" value={newToken} readonly class="input w-full pr-12! font-mono text-[12px] text-black dark:text-fg" />
              <button type="button" title="Copy token" aria-label="Copy token" class="icon-button group shrink-0 absolute top-1/2 right-2 -translate-y-1/2" onclick={copyToken}>
                <span class="inline-flex transition-transform duration-150 ease-out group-active:scale-75">
                  {#if copied}<Reicon name="check" class="size-3.5 text-success" />{:else}<Reicon name="copy" class="size-3.5" />{/if}
                </span>
              </button>
            </div>
          </div>
        </SettingsSection>
      {/if}

      <SettingsSection title="Issued tokens" flush>
        {#if tokens}
          <div>
            {#if tokens.length > 1}
              <div class="border-b border-neutral-200 p-3 dark:border-white/[0.08]">
                <div class="relative max-w-sm">
                  <Reicon name="search" class="pointer-events-none absolute top-1/2 left-2.5 z-10 size-3.5 -translate-y-1/2 text-neutral-400 dark:text-fg-faint" />
                  <input
                    bind:value={search}
                    oninput={() => (page = 1)}
                    type="search"
                    placeholder="Search tokens"
                    aria-label="Search tokens"
                    class="h-8! w-full rounded-lg! border-neutral-200! bg-white! py-0! pr-8! pl-8! text-[12px]! shadow-none! placeholder:text-neutral-400 focus:border-accent! focus:ring-0! dark:border-white/[0.08]! dark:bg-white/[0.035]! dark:text-fg! dark:placeholder:text-fg-faint"
                  />
                  {#if search}
                    <button
                      onclick={() => ((search = ''), (page = 1))}
                      type="button"
                      class="absolute top-1/2 right-2 flex size-5 -translate-y-1/2 items-center justify-center rounded text-neutral-400 transition-colors hover:bg-neutral-100 hover:text-black dark:text-fg-faint dark:hover:bg-white/[0.07] dark:hover:text-fg"
                      aria-label="Clear search"
                    >
                      <Reicon name="x" class="size-3" />
                    </button>
                  {/if}
                </div>
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
                      <div class="min-w-0 truncate text-[12px] font-medium text-black dark:text-fg">{token.name}</div>
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
                          <StatusBadge label="Expired" type="error" />
                        {:else}
                          {dateOf(token.expires_at)}
                        {/if}
                      </div>
                      <div class="flex justify-end">
                        <button
                          type="button"
                          class="inline-flex h-7 items-center rounded-md px-2 text-[11px] font-medium text-error transition-colors hover:bg-error/10"
                          onclick={() => (revoking = token)}
                        >
                          Revoke
                        </button>
                      </div>
                    </div>
                  {/each}
                </div>
                <Pager total={filtered.length} bind:page bind:pageSize={perPage} storageKey="coolify.page-size.api-tokens" />
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
