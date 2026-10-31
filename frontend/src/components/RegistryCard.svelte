<script lang="ts">
  // Port of upstream livewire/server/docker-registries/server-registries.blade.php.
  import ConfirmModal from './ConfirmModal.svelte';
  import Modal from './Modal.svelte';
  import Reicon from './Reicon.svelte';
  import RegistryLogin from './RegistryLogin.svelte';
  import StatusBadge from './StatusBadge.svelte';
  import { api } from '../lib/api';
  import { link } from '../lib/router.svelte';
  import { failure } from '../lib/serverApi';
  import { notify } from '../lib/toast.svelte';

  let { server }: { server: { uuid: string; name: string; functional: boolean } } = $props();
  type User = { type: string; name: string; link: string | null };
  type Row = { registry: string; logged_in: boolean; source: string | null; username: string | null; used_by: User[] };
  let rows = $state<Row[]>([]);
  let loaded = $state(false);
  let error = $state<string | null>(null);
  let checks = $state<Record<string, { ok: boolean; message: string }>>({});
  let expanded = $state<Record<string, boolean>>({});
  let modal = $state<{ title: string; subtitle: string; preset?: string; edit?: string; username?: string } | null>(null);
  let deleting = $state<string | null>(null);
  const base = $derived(`/servers/${encodeURIComponent(server.uuid)}/registries`);

  async function load() {
    checks = {};
    try {
      const d = await api.get<{ registries: Row[]; error: string | null }>(base);
      rows = d.registries;
      error = d.error;
      loaded = true;
    } catch (e) {
      failure(e);
    }
  }
  load();

  const hub = $derived(rows.find((r) => r.registry === 'docker.io' && !r.logged_in));
  const visible = $derived(rows.filter((r) => r !== hub));
  const plural = (word: string, n: number) => (n === 1 ? word : `${word}s`);
  const summary = (users: User[]) => {
    const counts = new Map<string, number>();
    for (const u of users) counts.set(u.type, (counts.get(u.type) ?? 0) + 1);
    return [...counts].map(([t, c]) => `${c} ${plural(t.toLowerCase(), c)}`).join(' · ');
  };
  const groups = (users: User[]) => {
    const map = new Map<string, User[]>();
    for (const u of users) map.set(u.type, [...(map.get(u.type) ?? []), u]);
    return [...map];
  };

  async function test(registry: string) {
    try {
      await api.post(`${base}/${encodeURIComponent(registry)}/check`, {});
      checks[registry] = { ok: true, message: 'The login works.' };
      notify.success(`The login for ${registry} works.`);
    } catch (e) {
      const message = e instanceof Error ? e.message : String(e);
      checks[registry] = { ok: false, message };
      notify.error(`The login for ${registry} does not work.`, message);
    }
  }
  async function logout() {
    if (!deleting) return;
    const registry = deleting;
    try {
      await api.del(`${base}/${encodeURIComponent(registry)}`);
      notify.success(`Deleted the login for ${registry}.`);
      await load();
    } catch (e) {
      notify.error('Logout failed.', e instanceof Error ? e.message : String(e));
    }
  }
</script>

{#snippet usedBy(user: User, withType = true)}
  <span class="inline-flex max-w-full min-w-0 items-baseline gap-1">
    {#if user.link}
      <a href={user.link} use:link class="truncate hover:underline">{user.name}</a>
    {:else}
      <span class="truncate">{user.name}</span>
    {/if}
    {#if withType}
      <span class="shrink-0 text-[11px] text-neutral-400 dark:text-fg-faint">{user.type}</span>
    {/if}
  </span>
{/snippet}

<div class="overflow-hidden rounded-xl border border-neutral-200 bg-white shadow-sm dark:border-white/[0.08] dark:bg-white/[0.05]">
  <div class="flex items-center gap-3 border-b border-neutral-200 bg-neutral-50 px-4 py-2.5 dark:border-white/[0.08] dark:bg-white/[0.05]">
    <Reicon name="servers" class="size-4 shrink-0 text-neutral-500 dark:text-fg-dim" />
    <a href="/server/{server.uuid}" use:link class="min-w-0 truncate text-[13px] font-semibold text-black hover:underline dark:text-fg">{server.name}</a>
    <div class="ml-auto flex shrink-0 items-center gap-2">
      <button type="button" class="button w-fit" onclick={load}>
        <Reicon name="refresh" class="size-3.5" />
        Refresh
      </button>
      {#if server.functional}
        <button type="button" class="button" onclick={() => (modal = { title: 'Log in to a registry', subtitle: `Runs docker login on ${server.name}.` })}>New</button>
      {/if}
    </div>
  </div>

  {#if error}
    <div class="px-4 py-3 text-[12px] text-red-700 dark:text-red-300">{error}</div>
  {/if}

  {#if visible.length === 0}
    {#if !error && loaded}
      <div class="px-4 py-3 text-[12px] text-neutral-500 dark:text-fg-faint">Not logged in to any registry, and no resource on this server uses a private registry host.</div>
    {/if}
  {:else}
    <div class="overflow-x-auto">
      <div class="grid min-w-[760px] grid-cols-[minmax(0,1fr)_9rem_minmax(0,1fr)_14rem] border-b border-neutral-200 px-4 py-2 text-[11px] font-medium text-neutral-500 dark:border-white/[0.07] dark:text-fg-faint">
        <div>Registry</div>
        <div>Status</div>
        <div>Used by</div>
        <div></div>
      </div>
      {#each visible as row (row.registry)}
        <div class="min-w-[760px] border-b border-neutral-200 last:border-b-0 dark:border-white/[0.07]">
          <div class="grid min-h-11 grid-cols-[minmax(0,1fr)_9rem_minmax(0,1fr)_14rem] items-center px-4 py-2 text-[12px]">
            <div class="truncate font-mono">{row.registry}</div>
            <div>
              {#if row.logged_in && checks[row.registry]?.ok === false}
                <span title={checks[row.registry].message}><StatusBadge type="error" label="Login failed" /></span>
              {:else if row.logged_in}
                <StatusBadge type="success" label={row.source === 'credHelpers' ? 'Credential helper' : 'Logged in'} />
              {:else if error}
                <StatusBadge label="Unknown" />
              {:else}
                <StatusBadge type={row.registry === 'docker.io' ? 'neutral' : 'warning'} label="Not logged in" />
              {/if}
            </div>
            <div class="min-w-0 text-neutral-600 dark:text-fg-dim">
              {#if row.used_by.length === 0}
                <span class="text-neutral-400 dark:text-fg-faint">-</span>
              {:else if row.used_by.length === 1}
                {@render usedBy(row.used_by[0])}
              {:else}
                <button type="button" aria-expanded={!!expanded[row.registry]} onclick={() => (expanded[row.registry] = !expanded[row.registry])} class="inline-flex max-w-full items-center gap-1 hover:text-black dark:hover:text-fg">
                  <span class="truncate">{summary(row.used_by)}</span>
                  <Reicon name="chevron-down" class="size-3 shrink-0 transition-transform {expanded[row.registry] ? 'rotate-180' : ''}" />
                </button>
              {/if}
            </div>
            <div class="flex justify-end gap-2">
              {#if row.logged_in}
                <button type="button" class="button w-fit" title="Run docker login with the saved credentials" onclick={() => test(row.registry)}>Test</button>
              {:else if !error && server.functional}
                <button type="button" class="button" onclick={() => (modal = { title: `Log in to ${row.registry}`, subtitle: `Runs docker login on ${server.name}.`, preset: row.registry })}>Log in</button>
              {/if}
              {#if row.logged_in && row.source === 'auths'}
                <button type="button" class="button" onclick={() => (modal = { title: `Edit login for ${row.registry}`, subtitle: `Runs docker login on ${server.name} again. The new values replace the old login.`, edit: row.registry, username: row.username ?? '' })}>Edit</button>
                <button type="button" class="button button-error" onclick={() => (deleting = row.registry)}>Delete</button>
              {/if}
            </div>
          </div>
          {#if row.used_by.length > 1 && expanded[row.registry]}
            <div class="grid gap-x-6 gap-y-3 bg-neutral-50 px-4 py-3 text-[12px] sm:grid-cols-3 dark:bg-white/[0.02]">
              {#each groups(row.used_by) as [type, users] (type)}
                <div class="min-w-0">
                  <p class="mb-1 text-[11px] font-medium text-neutral-500 dark:text-fg-faint">{plural(type, 2)} ({users.length})</p>
                  <ul class="flex flex-col gap-0.5 text-neutral-600 dark:text-fg-dim">
                    {#each users as user (user.name + user.link)}
                      <li class="truncate">{@render usedBy(user, false)}</li>
                    {/each}
                  </ul>
                </div>
              {/each}
            </div>
          {/if}
        </div>
      {/each}
    </div>
  {/if}

  {#if hub && !error}
    <div class="border-t border-neutral-200 px-4 py-2.5 text-[12px] text-neutral-500 dark:border-white/[0.07] dark:text-fg-faint">
      {hub.used_by.length}
      {hub.used_by.length === 1 ? 'resource on this server uses' : 'resources on this server use'}
      Docker Hub images. Public images need no login. Log in to Docker Hub for private images or higher pull limits.
    </div>
  {/if}
</div>

{#if modal}
  <Modal title={modal.title} subtitle={modal.subtitle} onclose={() => (modal = null)}>
    <RegistryLogin servers={[server]} editRegistry={modal.edit} presetRegistry={modal.preset} currentUsername={modal.username} ondone={() => ((modal = null), load())} />
  </Modal>
{/if}
{#if deleting}
  <ConfirmModal
    title="Delete the login for {deleting}?"
    actions={[`Runs docker logout on ${server.name} and removes the saved credentials.`, 'The server can no longer pull private images from this registry.', 'Deployments that need this registry fail until you log in again.']}
    confirmWithText={false}
    onconfirm={logout}
    onclose={() => (deleting = null)}
  />
{/if}
