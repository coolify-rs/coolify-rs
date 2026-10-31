<script lang="ts">
  // Port of upstream livewire/server/docker-registries/login.blade.php.
  import FormInput from './FormInput.svelte';
  import FormTextarea from './FormTextarea.svelte';
  import Listbox from './Listbox.svelte';
  import { api, ApiError } from '../lib/api';
  import { notify } from '../lib/toast.svelte';
  import { REGISTRY_PROVIDERS } from '../lib/registryProviders';

  let {
    servers,
    picker = false,
    editRegistry,
    presetRegistry,
    currentUsername = '',
    ondone,
  }: {
    servers: { uuid: string; name: string }[];
    /** The overview page: the user picks the servers. */
    picker?: boolean;
    editRegistry?: string;
    presetRegistry?: string;
    currentUsername?: string;
    ondone: () => void;
  } = $props();

  // svelte-ignore state_referenced_locally
  const known = editRegistry ?? presetRegistry;
  const providerFor = (r?: string) => Object.entries(REGISTRY_PROVIDERS).find(([, p]) => r && p.registry === r)?.[0] ?? 'custom';
  let provider = $state(providerFor(known));
  let selected = $state<string>('');
  let registry = $state(known ?? REGISTRY_PROVIDERS[providerFor(known)].registry);
  // svelte-ignore state_referenced_locally
  let username = $state(currentUsername);
  let password = $state('');
  let error = $state<ApiError | null>(null);
  let saving = $state(false);
  const preset = $derived(REGISTRY_PROVIDERS[provider]);

  function changeProvider(next: string) {
    const p = REGISTRY_PROVIDERS[next];
    registry = p.registry;
    username = p.username;
  }
  async function submit(e: Event) {
    e.preventDefault();
    saving = true;
    error = null;
    const targets = picker ? servers.filter((s) => s.uuid === selected) : servers;
    try {
      for (const s of targets) {
        await api.post(`/servers/${encodeURIComponent(s.uuid)}/registries`, { registry, username, password });
      }
      notify.success(editRegistry ? `Updated the login for ${registry}.` : `Logged in to ${registry}.`);
      ondone();
    } catch (err) {
      if (err instanceof ApiError && Object.keys(err.errors).length) error = err;
      else notify.error('Login failed.', err instanceof Error ? err.message : String(err));
    } finally {
      saving = false;
    }
  }
</script>

<div>
  <form class="flex flex-col gap-3" onsubmit={submit}>
    {#if picker}
      <Listbox id="selectedServers" label="Servers" required searchable placeholder={servers.length ? 'Select servers' : 'No reachable server to log in to'} searchPlaceholder="Search servers…" disabled={!servers.length} bind:value={selected} options={servers.map((s) => ({ value: s.uuid, label: s.name }))} />
    {/if}
    {#if !editRegistry}
      <Listbox id="provider" label="Provider" bind:value={provider} onchange={changeProvider} options={Object.entries(REGISTRY_PROVIDERS).map(([value, p]) => ({ value, label: p.label }))} />
    {/if}
    {#if preset.hint}
      <p class="text-[12px] text-neutral-500 dark:text-fg-dim">{preset.hint}</p>
    {/if}
    <div class="grid gap-3 md:grid-cols-2">
      <FormInput id="registry" label="Registry" required placeholder={preset.placeholder} readonly={!!editRegistry || preset.registry !== ''} bind:value={registry} error={error?.field('registry')} />
      <FormInput id="username" label="Username" required bind:value={username} error={error?.field('username')} />
    </div>
    {#if provider === 'google'}
      <FormTextarea id="password" label="Service account JSON key" rows={6} required monospace bind:value={password} error={error?.field('password')} />
    {:else}
      <FormInput id="password" type="password" label="Password or token" required bind:value={password} error={error?.field('password')} />
    {/if}
    <p class="text-[12px] text-neutral-500 dark:text-fg-dim">
      Coolify does not save the token. Docker saves the login on the server in <code>~/.docker/config.json</code>, encoded but not encrypted.
    </p>
    <div class="flex justify-end">
      <button type="submit" class="button button-highlighted" disabled={saving}>{editRegistry ? 'Update login' : 'Log in'}</button>
    </div>
  </form>
</div>
