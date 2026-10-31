<script lang="ts">
  // Port of upstream livewire/terminal/index.blade.php (App\Livewire\Terminal\Index). The target list
  // is complete. A selected target opens the session frame without a shell: Project\Shared\Terminal
  // and upstream's websocket service are not ported.
  import Empty from '../components/Empty.svelte';
  import ErrorPage from '../components/ErrorPage.svelte';
  import Helper from '../components/Helper.svelte';
  import Reicon from '../components/Reicon.svelte';
  import SettingsSection from '../components/SettingsSection.svelte';
  import { ApiError, request } from '../lib/api';
  import { notify } from '../lib/toast.svelte';

  document.title = 'Terminal | coolify-rs';

  type Target = { value: string; label: string; name: string; server: string; type: 'server' | 'container' };

  let servers = $state<{ uuid: string; name: string }[]>([]);
  let containers = $state<{ uuid: string; name: string; server_uuid: string }[]>([]);
  let loading = $state(true);
  let forbidden = $state(false);
  let search = $state('');
  let selected = $state<Target | null>(null);

  async function load() {
    try {
      const s = await request<{ servers: { uuid: string; name: string }[] }>('GET', '/ui/terminal');
      servers = s.servers;
      const c = await request<{ containers: typeof containers }>('GET', '/ui/terminal/containers');
      containers = c.containers;
    } catch (e) {
      if (e instanceof ApiError && e.status === 403) forbidden = true;
      else notify.failure(e instanceof ApiError ? e.message : String(e));
    } finally {
      loading = false;
    }
  }
  load();

  const targets = $derived.by(() => {
    const out: Target[] = [];
    for (const s of servers) {
      out.push({ value: s.uuid, label: `${s.name} · Server`, name: s.name, server: s.name, type: 'server' });
      for (const c of containers) {
        if (c.server_uuid === s.uuid) out.push({ value: c.uuid, label: `${s.name} · ${c.name}`, name: c.name, server: s.name, type: 'container' });
      }
    }
    return out;
  });

  const groups = $derived.by(() => {
    const q = search.trim().toLowerCase();
    const list = q ? targets.filter((t) => t.label.toLowerCase().includes(q)) : targets;
    return [
      { type: 'server', label: 'Servers', targets: list.filter((t) => t.type === 'server') },
      { type: 'container', label: 'Containers', targets: list.filter((t) => t.type === 'container') },
    ].filter((g) => g.targets.length > 0);
  });
</script>

{#if forbidden}
  <ErrorPage code={403} />
{:else}
  <div class="{selected ? 'terminal-page' : ''} application-settings-form">
    <header class="terminal-page-header shrink-0">
      <div class="flex items-center gap-2">
        <h1 class="text-[24px]! leading-7! font-semibold! tracking-tight!">Terminal</h1>
        <Helper helper="If you cannot connect, confirm the server is reachable and that the terminal port is open on the firewall.<br><br><a class='underline' href='https://coolify.io/docs/knowledge-base/server/firewall/#terminal' target='_blank' rel='noopener noreferrer'>Documentation</a>" />
      </div>
      <p class="mt-1 text-[13px] text-neutral-500 dark:text-fg-dim">Run commands on reachable servers and containers from the browser.</p>
    </header>

    <section class={selected ? 'terminal-page-console min-h-0 w-full flex-1 overflow-hidden' : 'w-full'}>
      {#if !selected}
        <div class="application-settings-workspace flex w-full min-w-0 flex-col" data-terminal-target-canvas>
          <SettingsSection title="Start a terminal session" flush>
            {#snippet actions()}
              {#if !loading && servers.length > 0}
                <div class="relative w-full sm:w-64">
                  <Reicon name="search" class="pointer-events-none absolute top-1/2 left-2.5 z-10 size-3.5 -translate-y-1/2 text-neutral-400 dark:text-fg-faint" />
                  <input
                    bind:value={search}
                    type="search"
                    placeholder="Filter targets"
                    aria-label="Filter terminal targets"
                    class="h-8! w-full rounded-lg! border-neutral-200! bg-white! py-0! pr-8! pl-8! text-[12px]! shadow-none! placeholder:text-neutral-400 focus:border-accent! focus:ring-0! dark:border-white/[0.08]! dark:bg-white/[0.035]! dark:text-fg! dark:placeholder:text-fg-faint"
                  />
                  {#if search}
                    <button
                      type="button"
                      onclick={() => (search = '')}
                      class="absolute top-1/2 right-2 flex size-5 -translate-y-1/2 items-center justify-center rounded text-neutral-400 transition-colors hover:bg-neutral-100 hover:text-black dark:text-fg-faint dark:hover:bg-white/[0.07] dark:hover:text-fg"
                      aria-label="Clear target filter"
                    >
                      <span class="text-sm leading-none">×</span>
                    </button>
                  {/if}
                </div>
              {/if}
            {/snippet}
            {#if loading}
              <div class="flex min-h-40 items-center justify-center gap-2 text-[13px] text-neutral-500 dark:text-fg-dim">
                <svg class="size-4 animate-spin" viewBox="0 0 24 24" fill="none" aria-hidden="true">
                  <circle class="opacity-25" cx="12" cy="12" r="9" stroke="currentColor" stroke-width="3" />
                  <path class="opacity-75" d="M21 12a9 9 0 0 0-9-9" stroke="currentColor" stroke-width="3" stroke-linecap="round" />
                </svg>
                <span>Finding available servers and containers…</span>
              </div>
            {:else if servers.length === 0}
              <Empty title="No terminal targets available" description="Connect a reachable server and enable terminal access to start a session." iconName="browser-terminal" />
            {:else}
              <div class="terminal-target-card-list">
                {#each groups as group (group.type)}
                  <section class="command-palette-section">
                    <div class="terminal-target-group-label">
                      <span>{group.label}</span>
                      <span class="terminal-target-group-count">{group.targets.length}</span>
                    </div>
                    {#each group.targets as target (target.value)}
                      <button type="button" class="command-palette-item cursor-pointer" onclick={() => (selected = target)}>
                        <Reicon name={target.type === 'server' ? 'servers' : 'layers'} class="size-3.5 shrink-0 text-neutral-400 dark:text-fg-faint" />
                        <span class="command-palette-item-name min-w-0 flex-1">{target.name}</span>
                        {#if servers.length > 1}
                          <span class="terminal-target-item-server">{target.server}</span>
                        {/if}
                        <Reicon name="arrow-right" class="command-palette-item-chevron" />
                      </button>
                    {/each}
                  </section>
                {/each}
                {#if groups.length === 0}
                  <div class="px-3 py-8 text-center text-[13px] text-neutral-500 dark:text-fg-dim">No matching targets</div>
                {/if}
              </div>
            {/if}
          </SettingsSection>
        </div>
      {:else}
        <div class="application-console-shell flex h-full min-h-0 flex-col overflow-hidden rounded-lg" data-terminal-session-canvas data-console-theme="system">
          <header class="terminal-session-toolbar flex items-center gap-3 text-white select-none">
            <div class="relative flex min-w-0 flex-1 items-center gap-2">
              <button
                type="button"
                class="terminal-session-target-trigger flex h-8 min-w-0 max-w-sm cursor-pointer items-center gap-2 rounded-md px-2.5 text-left text-xs font-medium text-white/70 transition-colors hover:bg-white/[0.08] hover:text-white"
                aria-label="Choose terminal target"
                onclick={() => (selected = null)}
              >
                <span class="min-w-0 truncate text-[11px] font-semibold text-white/80">{selected.label}</span>
              </button>
            </div>
          </header>
          <div class="terminal-session-panel flex min-h-0 flex-1 flex-col overflow-hidden p-3 sm:p-6">
            <div class="application-console-block min-h-0 flex-1 overflow-hidden">
              <div class="flex h-full min-h-0 items-center justify-center bg-[#141414] px-4">
                <Empty size="lg" title="The terminal session is not available yet" description="coolify-rs lists the targets, but does not open a shell from the browser yet." iconName="browser-terminal" />
              </div>
            </div>
          </div>
        </div>
      {/if}
    </section>
  </div>
{/if}
