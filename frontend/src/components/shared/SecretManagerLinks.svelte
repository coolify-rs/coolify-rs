<script lang="ts">
  // Port of upstream livewire/project/shared/secret-manager-links.blade.php.
  import ConfirmModal from '../ConfirmModal.svelte';
  import Empty from '../Empty.svelte';
  import FormInput from '../FormInput.svelte';
  import Listbox from '../Listbox.svelte';
  import SettingsSection from '../SettingsSection.svelte';
  import { ApiError, request } from '../../lib/api';
  import { resPath, type ResourceKind } from '../../lib/sharedApi';
  import { notify } from '../../lib/toast.svelte';

  let { kind, uuid, onenvschange }: { kind: ResourceKind; uuid: string; onenvschange?: () => void } = $props();

  interface State {
    linked: boolean;
    can_update: boolean;
    available_tokens: { value: string; label: string }[];
    integration_token_uuid: string | null;
    provider: string | null;
    provider_name: string | null;
    token_name: string | null;
    source_summary: string | null;
    doppler_token_type: string | null;
    settings: Record<string, string>;
  }

  const removeSourceWarning = 'Existing {{vault.*}} reference variables will fail the next deployment until they are removed too.';
  const base = () => `${resPath(kind, uuid)}/secret-manager`;

  let data = $state<State | null>(null);
  let tokenUuid = $state('');
  let settings = $state<Record<string, string>>({});
  let errors = $state<Record<string, string>>({});
  let keys = $state<{ key: string; valid: boolean }[]>([]);
  let keysLoaded = $state(false);
  let search = $state('');
  let confirmRemove = $state(false);
  let busy = $state(false);

  const filteredKeys = $derived(search.trim() === '' ? keys : keys.filter((k) => k.key.toLowerCase().includes(search.toLowerCase())));

  function resetKeys() {
    keys = [];
    keysLoaded = false;
    search = '';
  }

  async function load() {
    const d = await request<State>('GET', base());
    data = d;
    tokenUuid = d.integration_token_uuid ?? '';
    settings = { ...d.settings };
    errors = {};
  }

  $effect(() => {
    load();
  });

  function fail(e: unknown) {
    if (e instanceof ApiError) {
      const fields = ['project', 'config', 'project_id', 'environment', 'secret_path', 'mount', 'path'].map((f) => `settings.${f}`);
      const found = Object.fromEntries(fields.map((f) => [f, e.field(f)]).filter(([, m]) => m));
      if (Object.keys(found).length > 0) {
        errors = found as Record<string, string>;
        return;
      }
      notify.failure(e.message);
    }
  }

  async function changeToken(value: string) {
    tokenUuid = value;
    try {
      const r = await request<{ saved: boolean; message?: string }>('PUT', base(), { integration_token_uuid: value });
      if (!r.saved) return;
      resetKeys();
      await load();
      if (r.message) notify.success(r.message);
    } catch (e) {
      fail(e);
    }
  }

  async function saveSettings() {
    errors = {};
    try {
      const r = await request<{ message?: string }>('POST', `${base()}/settings`, { settings: { ...settings } });
      resetKeys();
      await load();
      if (r.message) notify.success(r.message);
    } catch (e) {
      fail(e);
    }
  }

  async function removeSource() {
    confirmRemove = false;
    try {
      const r = await request<{ message: string }>('DELETE', base());
      tokenUuid = '';
      settings = {};
      resetKeys();
      await load();
      notify.success(r.message);
    } catch (e) {
      fail(e);
    }
  }

  async function loadKeys() {
    busy = true;
    try {
      const r = await request<{ keys: { key: string; valid: boolean }[]; loaded: boolean }>('POST', `${base()}/keys`);
      keys = r.keys;
      keysLoaded = r.loaded;
    } catch (e) {
      fail(e);
    } finally {
      busy = false;
    }
  }

  async function importAll() {
    busy = true;
    try {
      const r = await request<{ message: string }>('POST', `${base()}/import`);
      onenvschange?.();
      notify.success(r.message);
    } catch (e) {
      fail(e);
    } finally {
      busy = false;
    }
  }

  async function addReference(key: string) {
    try {
      const r = await request<{ added: boolean; message?: string }>('POST', `${base()}/references`, { key });
      if (r.added) {
        onenvschange?.();
        if (r.message) notify.success(r.message);
      }
    } catch (e) {
      fail(e);
    }
  }
</script>

{#snippet field(id: string, label: string, required: boolean, placeholder?: string)}
  <FormInput
    {required}
    id={`settings.${id}`}
    {label}
    {placeholder}
    error={errors[`settings.${id}`]}
    value={settings[id] ?? ''}
    oninput={(e: Event) => (settings[id] = (e.target as HTMLInputElement).value)}
    onblur={saveSettings}
  />
{/snippet}

{#if data}
  <div class="mt-8">
    <SettingsSection
      title="Secret manager"
      description={'Reference remote secrets in your environment variables with {{vault.KEY}}. Values are fetched at deployment time and are never stored in the coolify-rs database. Changing the source does not re-check existing references — missing keys fail the next deployment.'}
    >
      {#if !data.linked && data.available_tokens.length === 0}
        <Empty title="No secret manager tokens" description="Add a Doppler, Infisical, or HashiCorp Vault token first." iconName="keys" size="sm">
          {#snippet contents()}
            <a class="button" href="/security/integration-tokens">Integration tokens</a>
          {/snippet}
        </Empty>
      {:else if data.can_update}
        <div class="application-settings-form flex w-full flex-col gap-4">
          <div class="flex items-end gap-2">
            <Listbox
              id="integration_token_uuid"
              label="Integration token"
              placeholder="Select a secret manager token"
              options={data.available_tokens}
              value={tokenUuid}
              onchange={(v: string) => changeToken(v)}
            />
            {#if data.linked}
              <button type="button" class="button button-error" onclick={() => (confirmRemove = true)}>Remove</button>
            {/if}
          </div>

          {#if data.linked}
            {#if data.provider === 'doppler'}
              {#if data.doppler_token_type === 'service_account'}
                <div class="grid gap-4 lg:grid-cols-2">
                  {@render field('project', 'Project (required)', true)}
                  {@render field('config', 'Config (required)', true, 'prd')}
                </div>
              {:else}
                <p class="text-[12px] text-neutral-500 dark:text-fg-dim">Project and config are fixed by this service token.</p>
              {/if}
            {:else if data.provider === 'infisical'}
              <div class="grid gap-4 lg:grid-cols-3">
                {@render field('project_id', 'Project ID', true)}
                {@render field('environment', 'Environment slug', true, 'prod')}
                {@render field('secret_path', 'Secret path', false, '/')}
              </div>
            {:else if data.provider === 'vault'}
              <div class="grid gap-4 lg:grid-cols-2">
                {@render field('mount', 'KV v2 mount', true, 'secret')}
                {@render field('path', 'Secret path', true, 'my-app/production')}
              </div>
            {/if}

            <div class="flex flex-col gap-3">
              <div class="flex items-center gap-2">
                <button type="button" class="button" disabled={busy} onclick={loadKeys}>
                  {keysLoaded ? 'Reload keys' : 'Browse keys'}
                </button>
                {#if keysLoaded}
                  <button type="button" class="button button-highlighted" disabled={busy} onclick={importAll}>Import all keys</button>
                {/if}
              </div>

              {#if keysLoaded}
                {#if keys.length === 0}
                  <div class="text-[12px] text-neutral-500 dark:text-fg-dim">No secrets found at this source.</div>
                {:else}
                  <FormInput id="secret-key-search" label="Search keys" placeholder="Filter key names" bind:value={search} />
                  <div class="divide-y divide-neutral-200 rounded-lg border border-neutral-200 dark:divide-white/[0.07] dark:border-white/[0.08]">
                    {#each filteredKeys as k (k.key)}
                      <div class="flex items-center justify-between gap-3 px-3 py-2">
                        <div class="flex min-w-0 flex-col">
                          <span class="font-mono text-[12px] text-black dark:text-fg">{k.key}</span>
                          <span class="font-mono text-[11px] text-neutral-400 dark:text-fg-dim">{`{{vault.${k.key}}}`}</span>
                        </div>
                        {#if !k.valid}
                          <span class="shrink-0 text-[11px] text-neutral-500 dark:text-fg-dim">Not a valid variable name</span>
                        {:else}
                          <button type="button" class="button" onclick={() => addReference(k.key)}>Add as variable</button>
                        {/if}
                      </div>
                    {:else}
                      <div class="px-3 py-2 text-[12px] text-neutral-500 dark:text-fg-dim">No keys match the search.</div>
                    {/each}
                  </div>
                  <p class="text-[11px] text-neutral-500 dark:text-fg-dim">
                    Key names only — values stay in the secret manager. "Add as variable" creates
                    <span class="font-mono">{'KEY={{vault.KEY}}'}</span>; you can also paste a reference into any variable value, including inside a longer string.
                  </p>
                {/if}
              {/if}
            </div>
          {/if}
        </div>
      {:else if data.linked}
        <div class="px-1 py-2 text-[13px] text-black dark:text-fg">
          {data.provider_name}
          <span class="text-neutral-500 dark:text-fg-dim">{data.token_name} &middot; {data.source_summary}</span>
        </div>
      {/if}
    </SettingsSection>
  </div>
{/if}

{#if confirmRemove}
  <ConfirmModal
    title="Remove secret manager source?"
    actions={[removeSourceWarning]}
    confirmWithText={false}
    confirmWithPassword={false}
    step1ButtonText="Remove source"
    onconfirm={removeSource}
    onclose={() => (confirmRemove = false)}
  />
{/if}
