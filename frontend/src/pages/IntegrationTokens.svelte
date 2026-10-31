<script lang="ts">
  // Port of upstream livewire/security/integration-tokens.blade.php,
  // livewire/security/integration-token-form.blade.php and
  // livewire/security/integration-token-editor.blade.php.
  import Reicon from '../components/Reicon.svelte';
  import Empty from '../components/Empty.svelte';
  import Modal from '../components/Modal.svelte';
  import FormInput from '../components/FormInput.svelte';
  import Listbox from '../components/Listbox.svelte';
  import Checkbox from '../components/Checkbox.svelte';
  import ConfirmModal from '../components/ConfirmModal.svelte';
  import SecurityLayout from '../components/SecurityLayout.svelte';
  import SettingsSection from '../components/SettingsSection.svelte';
  import { ApiError } from '../lib/api';
  import { securityUi, type IntegrationToken } from '../lib/securityApi';
  import { notify } from '../lib/toast.svelte';

  document.title = 'Integration Tokens | coolify-rs';

  const PROVIDER_NAMES: Record<string, string> = { cloudflare: 'Cloudflare', doppler: 'Doppler', infisical: 'Infisical', vault: 'HashiCorp Vault' };
  const box = 'rounded-lg border border-neutral-200 bg-neutral-50 p-3 text-[11px] leading-5 text-neutral-600 dark:border-white/[0.08] dark:bg-white/[0.025] dark:text-fg-dim';
  const note = 'px-2.5 pb-2 text-[11px] text-neutral-500 dark:text-fg-dim';
  const rounded = 'rounded-lg border border-neutral-200 p-1 dark:border-white/[0.08]';

  let tokens = $state<IntegrationToken[] | null>(null);
  async function load() {
    tokens = (await securityUi.integrationTokens()).tokens;
  }
  load();

  /** `IntegrationToken::cloudflareTokenTemplateUrl`. */
  function templateUrl(capabilities: string[]): string {
    const tunnel = capabilities.includes('tunnel');
    const permissions = [
      { key: 'dns', type: 'edit' },
      { key: 'zone', type: 'read' },
      ...(tunnel ? [{ key: 'argotunnel', type: 'edit' }] : []),
    ];
    const query = [
      ['permissionGroupKeys', JSON.stringify(permissions)],
      ['accountId', '*'],
      ['zoneId', 'all'],
      ['name', tunnel ? 'Coolify DNS and Tunnel Management' : 'Coolify DNS Management'],
    ]
      .map(([k, v]) => `${k}=${encodeURIComponent(v).replace(/[!'()*]/g, (c) => `%${c.charCodeAt(0).toString(16).toUpperCase()}`)}`)
      .join('&');
    return `https://dash.cloudflare.com/profile/api-tokens?${query}`;
  }

  function withCapability(list: string[], cap: string, on: boolean): string[] {
    let next = on ? [...list, cap] : list.filter((c) => c !== cap);
    // `updatedCapabilities`: tunnel also selects DNS.
    if (next.includes('tunnel') && !next.includes('dns')) next = [...next, 'dns'];
    return next;
  }

  // New token.
  let adding = $state(false);
  let provider = $state('cloudflare');
  let name = $state('');
  let token = $state('');
  let capabilities = $state<string[]>(['dns']);
  // Bound fields need a value; the server drops the blank ones (`array_filter`).
  const emptyMetadata = { base_url: '', client_id: '', namespace: '', timeout: '' };
  let metadata = $state<Record<string, any>>({ ...emptyMetadata });
  let automaticDns = $state(true);
  let proxiedDns = $state(false);
  let addError = $state<ApiError | null>(null);

  function providerChanged() {
    if (provider === 'cloudflare') {
      capabilities = ['dns'];
      metadata = { ...emptyMetadata };
      automaticDns = true;
      proxiedDns = false;
    } else {
      capabilities = ['secrets'];
      metadata = { ...emptyMetadata, ...(provider === 'infisical' ? { base_url: 'https://app.infisical.com' } : {}) };
    }
  }

  async function addToken(e: SubmitEvent) {
    e.preventDefault();
    addError = null;
    try {
      const r = await securityUi.addIntegrationToken({ provider, name, token, capabilities, metadata: $state.snapshot(metadata), automatic_dns: automaticDns, proxied_dns: proxiedDns });
      name = '';
      token = '';
      adding = false;
      notify.success(r.message);
      await load();
    } catch (err) {
      const error = err instanceof ApiError ? err : new ApiError(0, String(err));
      if (Object.keys(error.errors).length) addError = error;
      else notify.failure(error.message);
    }
  }

  // Editor.
  let editing = $state<IntegrationToken | null>(null);
  let editName = $state('');
  let newToken = $state('');
  let editCaps = $state<string[]>([]);
  let editMeta = $state<Record<string, any>>({});
  let editAuto = $state(true);
  let editProxied = $state(false);
  let editError = $state<ApiError | null>(null);
  let deleting = $state(false);

  function openEditor(t: IntegrationToken) {
    editing = t;
    editName = t.name;
    newToken = '';
    editCaps = [...(t.capabilities ?? [])];
    editMeta = { ...emptyMetadata, ...(t.metadata ?? {}) };
    editAuto = t.provider === 'cloudflare' && t.metadata?.automatic_dns !== false;
    editProxied = t.provider === 'cloudflare' && t.metadata?.proxied_dns === true;
    editError = null;
  }

  async function save(e: SubmitEvent) {
    e.preventDefault();
    if (!editing) return;
    editError = null;
    try {
      const r = await securityUi.updateIntegrationToken(editing.uuid, {
        name: editName,
        new_token: newToken,
        capabilities: editCaps,
        metadata: $state.snapshot(editMeta),
        automatic_dns: editAuto,
        proxied_dns: editProxied,
      });
      newToken = '';
      notify.success(r.message);
      await load();
      const fresh = tokens?.find((t) => t.uuid === editing?.uuid);
      if (fresh) editing = fresh;
    } catch (err) {
      const error = err instanceof ApiError ? err : new ApiError(0, String(err));
      if (Object.keys(error.errors).length) editError = error;
      else notify.failure(error.message);
    }
  }

  async function refreshZones() {
    if (!editing) return;
    try {
      notify.success((await securityUi.refreshZones(editing.uuid)).message);
    } catch (err) {
      notify.failure(err instanceof ApiError ? err.message : String(err));
    }
  }

  async function remove() {
    if (!editing) return;
    try {
      const r = await securityUi.deleteIntegrationToken(editing.uuid);
      deleting = false;
      editing = null;
      notify.success(r.message);
      await load();
    } catch (err) {
      deleting = false;
      notify.failure(err instanceof ApiError ? err.message : String(err));
    }
  }
</script>

<div>
  <SecurityLayout>
    <div class="application-settings-form">
      <SettingsSection title="Integration tokens" description="Credentials used by third-party integrations such as DNS providers and secret managers." flush>
        {#snippet actions()}
          <button type="button" class="button button-highlighted" onclick={() => (adding = true)}>
            <Reicon name="plus" class="size-3.5" />
            New token
          </button>
        {/snippet}
        {#if tokens}
          {#if tokens.length === 0}
            <Empty title="No integration tokens" description="Add a provider token to connect a third-party integration." iconName="keys" size="sm" />
          {:else}
            <div class="divide-y divide-neutral-200 dark:divide-white/[0.07]">
              {#each tokens as t (t.uuid)}
                <div>
                  <div class="border-b border-neutral-200 last:border-b-0 dark:border-white/[0.07]">
                    <div class="grid min-h-14 w-full grid-cols-[minmax(0,1fr)_8rem_minmax(0,1fr)_2rem] items-center gap-3 px-4 py-2.5 text-left transition-colors hover:bg-neutral-50 dark:hover:bg-white/[0.025]">
                      <div class="min-w-0">
                        <h3 class="truncate text-[13px]! font-semibold! text-black dark:text-fg"><span>{t.name}</span></h3>
                      </div>
                      <div class="text-center text-[12px] text-neutral-500 dark:text-fg-dim">{PROVIDER_NAMES[t.provider] ?? t.provider}</div>
                      <div class="flex flex-wrap gap-1">
                        {#each t.capabilities ?? [] as capability (capability)}
                          <span class="rounded-full bg-neutral-100 px-2 py-0.5 text-[10px] font-medium uppercase text-neutral-600 dark:bg-white/[0.06] dark:text-fg-dim">{capability}</span>
                        {/each}
                        {#if t.provider === 'cloudflare'}
                          {#if t.metadata?.automatic_dns !== false}
                            <span class="rounded-full bg-green-100 px-2 py-0.5 text-[10px] font-medium uppercase text-green-700 dark:bg-green-500/10 dark:text-green-400">Auto DNS</span>
                          {/if}
                          <span class="text-[11px] text-neutral-500 dark:text-fg-dim">{t.zones.length} {t.zones.length === 1 ? 'zone' : 'zones'}</span>
                        {/if}
                      </div>
                      <button type="button" class="icon-button" title="Edit integration token" aria-label="Edit {t.name}" onclick={() => openEditor(t)}>
                        <Reicon name="settings" class="size-3.5" />
                      </button>
                    </div>
                  </div>
                </div>
              {/each}
            </div>
          {/if}
        {/if}
      </SettingsSection>
    </div>
  </SecurityLayout>
</div>

{#if adding}
  <Modal title="New Integration Token" onclose={() => (adding = false)}>
    <div class="w-full">
      <form class="application-settings-form flex w-full flex-col gap-4" onsubmit={addToken}>
        <Listbox
          required
          id="provider"
          label="Provider"
          bind:value={provider}
          onchange={providerChanged}
          options={[
            { value: 'cloudflare', label: 'Cloudflare' },
            { value: 'doppler', label: 'Doppler' },
            { value: 'infisical', label: 'Infisical' },
            { value: 'vault', label: 'HashiCorp Vault' },
          ]}
        />

        {#if provider === 'infisical'}
          <div class="grid gap-4 lg:grid-cols-2">
            <FormInput required id="name" label="Token name" placeholder="Production secrets" bind:value={name} error={addError?.field('name')} />
            <FormInput required id="metadata.client_id" label="Client ID" placeholder="Machine identity client ID" bind:value={metadata.client_id} error={addError?.field('metadata.client_id')} />
            <FormInput required type="password" id="token" label="Client secret" placeholder="Paste the machine identity client secret" bind:value={token} error={addError?.field('token')} />
            <FormInput
              required
              id="metadata.base_url"
              label="Base URL"
              placeholder="https://app.infisical.com"
              helper="Use the URL of your self-hosted Infisical instance, or the Infisical cloud URL."
              bind:value={metadata.base_url}
              error={addError?.field('metadata.base_url')}
            />
            <FormInput
              type="number"
              min="1"
              max="300"
              id="metadata.timeout"
              label="Request timeout (optional)"
              placeholder="10"
              helper="Seconds to wait for each request. Increase it if your instance responds slowly."
              bind:value={metadata.timeout}
              error={addError?.field('metadata.timeout')}
            />
          </div>
        {:else}
          <div class="grid gap-4 lg:grid-cols-2">
            <FormInput required id="name" label="Token name" placeholder={provider === 'cloudflare' ? 'Production DNS' : 'Production secrets'} bind:value={name} error={addError?.field('name')} />
            <FormInput required type="password" id="token" label={provider === 'vault' ? 'Vault token' : 'API token'} placeholder="Paste the provider token" bind:value={token} error={addError?.field('token')} />
          </div>
        {/if}

        {#if provider === 'vault'}
          <div class="grid gap-4 lg:grid-cols-2">
            <FormInput required id="metadata.base_url" label="Base URL" placeholder="https://vault.example.com:8200" bind:value={metadata.base_url} error={addError?.field('metadata.base_url')} />
            <FormInput id="metadata.namespace" label="Namespace (optional)" placeholder="admin/team-a" helper="Only for Vault Enterprise / HCP Vault." bind:value={metadata.namespace} />
            <FormInput
              type="number"
              min="1"
              max="300"
              id="metadata.timeout"
              label="Request timeout (optional)"
              placeholder="10"
              helper="Seconds to wait for each request. Increase it if your instance responds slowly."
              bind:value={metadata.timeout}
              error={addError?.field('metadata.timeout')}
            />
          </div>
        {/if}

        {#if provider === 'cloudflare'}
          <fieldset>
            <legend class="text-sm font-medium text-black dark:text-fg">Capabilities</legend>
            <div class="mt-3 {rounded}">
              <Checkbox id="dns-capability" label="DNS" fullWidth disabled={capabilities.includes('tunnel')} checked={capabilities.includes('dns')} onchange={(c) => (capabilities = withCapability(capabilities, 'dns', c))} />
              <p class={note}>Manage Cloudflare DNS records.</p>
              <Checkbox id="tunnel-capability" label="Cloudflare Tunnel" fullWidth checked={capabilities.includes('tunnel')} onchange={(c) => (capabilities = withCapability(capabilities, 'tunnel', c))} />
              <p class={note}>Create and manage tunnels for server SSH. Needs DNS.</p>
            </div>
            {#if addError?.field('capabilities')}
              <span class="text-xs text-red-500">{addError.field('capabilities')}</span>
            {/if}
          </fieldset>
          <div class={rounded}>
            <Checkbox id="automatic-dns" label="Automatically configure DNS" fullWidth bind:checked={automaticDns} />
            <p class={note}>Create DNS records automatically when a new domain has one unambiguous matching credential.</p>
          </div>
          <div class={rounded}>
            <Checkbox id="proxied-dns" label="Proxy DNS records through Cloudflare" fullWidth bind:checked={proxiedDns} />
            <p class={note}>Create new DNS records with the Cloudflare proxy on (orange cloud), so the server IP is not public. Existing records keep their proxy status.</p>
          </div>
        {:else}
          <div class={box}>
            <div class="font-medium text-black dark:text-fg">Capability: Secrets (read-only)</div>
            <p>Coolify reads secrets from this provider at deployment time and writes them into the generated <code>.{'env'}</code> file. Secret values are never stored in the Coolify database.</p>
          </div>
        {/if}

        {#if provider === 'cloudflare' && capabilities.includes('dns')}
          <div class={box}>
            <div class="font-medium text-black dark:text-fg">Required Cloudflare permissions</div>
            <ul class="list-inside list-disc">
              <li>Zone - DNS - Edit</li>
              <li>Zone - Zone - Read</li>
              {#if capabilities.includes('tunnel')}
                <li>Account - Cloudflare Tunnel - Edit</li>
              {/if}
            </ul>
            <p>Limit zone resources to the zones Coolify should manage.</p>
            <a href={templateUrl(capabilities)} target="_blank" rel="noopener noreferrer" class="button mt-2">
              Create token at Cloudflare
              <svg class="inline-flex w-3 h-3 dark:text-neutral-400 text-black" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M10 6H6a2 2 0 00-2 2v10a2 2 0 002 2h10a2 2 0 002-2v-4M14 4h6m0 0v6m0-6L10 14" />
              </svg>
            </a>
          </div>
        {:else if provider === 'doppler'}
          <div class={box}>
            <div class="font-medium text-black dark:text-fg">Recommended Doppler token</div>
            <p>Use a read-only <span class="font-medium">Service Token</span> (dp.st.*). It is pinned to one project and config. Create it in Doppler under Project &gt; Config &gt; Access.</p>
          </div>
        {:else if provider === 'infisical'}
          <div class={box}>
            <div class="font-medium text-black dark:text-fg">Infisical machine identity</div>
            <p>Create a machine identity with Universal Auth and read access to your project. Paste the client ID above and the client secret in the secret field.</p>
          </div>
        {:else if provider === 'vault'}
          <div class={box}>
            <div class="font-medium text-black dark:text-fg">Vault token</div>
            <p>Use a token with read access to your KV v2 secrets. Prefer a periodic or long-lived token — deployments fail when the token expires.</p>
          </div>
        {/if}

        <div class="flex justify-end border-t border-neutral-200 pt-4 dark:border-white/[0.08]">
          <button type="submit" class="button" {...{ isHighlighted: '' }}>Validate and add</button>
        </div>
      </form>
    </div>
  </Modal>
{/if}

{#if editing}
  <Modal title="Edit Integration Token" focusFirstField={false} onclose={() => (editing = null)}>
    <div class="w-full">
      <form class="application-settings-form flex w-full flex-col gap-4" onsubmit={save}>
        <div class="grid gap-4 lg:grid-cols-2">
          <FormInput required id="name" label="Token name" bind:value={editName} error={editError?.field('name')} />
          <FormInput id="provider" readonly label="Provider" value={PROVIDER_NAMES[editing.provider] ?? editing.provider} />
          <div class="lg:col-span-2">
            <FormInput
              type="password"
              id="newToken"
              label={editing.provider === 'infisical' ? 'New client secret' : 'New API token'}
              placeholder="Leave blank to keep the current token"
              helper={editing.provider === 'infisical'
                ? 'Paste a replacement client secret to rotate this credential. Required when you change the base URL or the client ID.'
                : editing.provider === 'vault'
                  ? 'Paste a replacement token to rotate this credential. Required when you change the base URL or the namespace.'
                  : 'Paste a replacement token to rotate this credential.'}
              bind:value={newToken}
              error={editError?.field('new_token')}
            />
          </div>
        </div>

        {#if editing.provider === 'infisical'}
          <div class="grid gap-4 lg:grid-cols-2">
            <FormInput required id="metadata.base_url" label="Base URL" placeholder="https://app.infisical.com" bind:value={editMeta.base_url} error={editError?.field('metadata.base_url')} />
            <FormInput required id="metadata.client_id" label="Client ID" bind:value={editMeta.client_id} error={editError?.field('metadata.client_id')} />
            <FormInput
              type="number"
              min="1"
              max="300"
              id="metadata.timeout"
              label="Request timeout (optional)"
              placeholder="10"
              helper="Seconds to wait for each request. Increase it if your instance responds slowly."
              bind:value={editMeta.timeout}
              error={editError?.field('metadata.timeout')}
            />
          </div>
        {:else if editing.provider === 'vault'}
          <div class="grid gap-4 lg:grid-cols-2">
            <FormInput required id="metadata.base_url" label="Base URL" placeholder="https://vault.example.com:8200" bind:value={editMeta.base_url} error={editError?.field('metadata.base_url')} />
            <FormInput id="metadata.namespace" label="Namespace (optional)" bind:value={editMeta.namespace} />
            <FormInput
              type="number"
              min="1"
              max="300"
              id="metadata.timeout"
              label="Request timeout (optional)"
              placeholder="10"
              helper="Seconds to wait for each request. Increase it if your instance responds slowly."
              bind:value={editMeta.timeout}
              error={editError?.field('metadata.timeout')}
            />
          </div>
        {/if}

        {#if editing.provider === 'cloudflare'}
          <fieldset>
            <legend class="text-sm font-medium text-black dark:text-fg">Capabilities</legend>
            <div class="mt-3 {rounded}">
              <Checkbox id="edit-dns-capability" label="DNS" fullWidth disabled={editCaps.includes('tunnel')} checked={editCaps.includes('dns')} onchange={(c) => (editCaps = withCapability(editCaps, 'dns', c))} />
              <p class={note}>Manage Cloudflare DNS records.</p>
              <Checkbox id="edit-tunnel-capability" label="Cloudflare Tunnel" fullWidth checked={editCaps.includes('tunnel')} onchange={(c) => (editCaps = withCapability(editCaps, 'tunnel', c))} />
              <p class={note}>Create and manage tunnels for server SSH. Needs DNS.</p>
            </div>
            {#if editError?.field('capabilities')}
              <span class="text-xs text-red-500">{editError.field('capabilities')}</span>
            {/if}
          </fieldset>
          <div class={rounded}>
            <Checkbox id="edit-automatic-dns" label="Automatically configure DNS" fullWidth bind:checked={editAuto} />
            <p class={note}>Create DNS records automatically when a new domain has one unambiguous matching credential.</p>
          </div>
          <div class={rounded}>
            <Checkbox id="edit-proxied-dns" label="Proxy DNS records through Cloudflare" fullWidth bind:checked={editProxied} />
            <p class={note}>Create new DNS records with the Cloudflare proxy on (orange cloud), so the server IP is not public. Existing records keep their proxy status.</p>
          </div>
        {:else}
          <div class={box}>
            <div class="font-medium text-black dark:text-fg">Capability: Secrets (read-only)</div>
            <p>Coolify reads secrets from this provider at deployment time. Secret values are never stored in the Coolify database.</p>
          </div>
        {/if}

        {#if editing.provider === 'cloudflare' && editCaps.includes('dns')}
          <div class={box}>
            <div class="font-medium text-black dark:text-fg">Required Cloudflare permissions</div>
            <ul class="list-inside list-disc">
              <li>Zone - DNS - Edit</li>
              <li>Zone - Zone - Read</li>
              {#if editCaps.includes('tunnel')}
                <li>Account - Cloudflare Tunnel - Edit</li>
              {/if}
            </ul>
            <a href={templateUrl(editCaps)} target="_blank" rel="noopener noreferrer" class="font-medium text-coollabs hover:underline dark:text-brand-light">Create a replacement token in Cloudflare</a>
          </div>
          <div class="rounded-lg border border-neutral-200 dark:border-white/[0.08]">
            <div class="flex items-center justify-between gap-3 p-3">
              <div class="text-xs text-neutral-600 dark:text-fg-dim">
                <div class="font-medium text-black dark:text-fg">Domains this token can manage</div>
                <div>{editing.zones.length} accessible {editing.zones.length === 1 ? 'zone' : 'zones'}</div>
              </div>
              <button type="button" class="button" onclick={refreshZones}>Refresh zones</button>
            </div>
            {#if editing.zones.length > 0}
              <div class="max-h-48 overflow-y-auto border-t border-neutral-200 dark:border-white/[0.08]">
                {#each editing.zones as zone (zone.id)}
                  <div class="flex items-center justify-between gap-3 border-b border-neutral-200 px-3 py-2.5 last:border-b-0 dark:border-white/[0.08]">
                    <div class="min-w-0">
                      <div class="truncate text-sm font-medium text-black dark:text-fg">{zone.name}</div>
                      {#if zone.account_name}
                        <div class="truncate text-[11px] text-neutral-500 dark:text-fg-dim">{zone.account_name}</div>
                      {/if}
                    </div>
                    {#if zone.managed_records_count > 0}
                      <div class="shrink-0 text-[11px] text-neutral-500 dark:text-fg-dim">{zone.managed_records_count} managed {zone.managed_records_count === 1 ? 'record' : 'records'}</div>
                    {/if}
                  </div>
                {/each}
              </div>
            {/if}
          </div>
        {/if}

        <div class="flex items-center justify-between gap-2 border-t border-neutral-200 pt-4 dark:border-white/[0.08]">
          <button type="button" class="button button-error" onclick={() => (deleting = true)}>Delete</button>
          <button type="submit" class="button" {...{ isHighlighted: '' }}>Validate and save</button>
        </div>
      </form>
    </div>
  </Modal>
{/if}

{#if deleting && editing}
  <ConfirmModal
    title="Delete integration token?"
    actions={['This integration token will be permanently deleted.']}
    confirmationText={editing.name}
    step2ButtonText="Delete token"
    onconfirm={remove}
    onclose={() => (deleting = false)}
  />
{/if}
