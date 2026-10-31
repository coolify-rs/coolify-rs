<script lang="ts">
  // Port of upstream livewire/project/shared/execute-container-command.blade.php and
  // `Project\Shared\ExecuteContainerCommand`, on every resource kind and on a server. The heading and
  // the configuration sidebar come from the page around it.
  import Empty from '../Empty.svelte';
  import Reicon from '../Reicon.svelte';
  import TerminalSession from './TerminalSession.svelte';
  import TerminalToolbarSelectors from './TerminalToolbarSelectors.svelte';
  import { ApiError } from '../../lib/api';
  import { ConsoleChrome, consoleThemes } from '../../lib/terminal/chrome.svelte';
  import { terminalApi, type TerminalContainer, type TerminalKind, type TerminalServer } from '../../lib/terminalApi';
  import { notify } from '../../lib/toast.svelte';

  let { kind, uuid }: { kind: TerminalKind; uuid: string } = $props();

  const chrome = new ConsoleChrome();
  chrome.init();

  let servers = $state<TerminalServer[]>([]);
  let mounted = $state(false);
  let containers = $state<TerminalContainer[]>([]);
  let containersLoaded = $state(false);
  let selected = $state('default');
  let targetChosen = $state(false);
  let containerOpen = $state(false);
  let session = $state<TerminalSession>();
  let picker = $state<HTMLElement>();

  const first = $derived(servers[0]);
  const consoleUnavailable = $derived(
    kind === 'server' ? !first || !first.terminal_enabled || !first.functional : containersLoaded && containers.length === 0,
  );
  const selectedLabel = $derived(containers.find((c) => c.value === selected)?.label ?? 'Choose a container');

  chrome.onModeChange = (mode) => {
    // `Terminal::setMode()`: reconnects to the current container.
    if (selected === 'default') return;
    const server = selected.split(':')[0];
    const name = selected.slice(server.length + 1);
    terminalApi
      .session(true, name, server, mode)
      .then((a) => session?.apply(a))
      .catch((e) => session?.fail(message(e)));
  };

  function message(e: unknown): string {
    return e instanceof ApiError ? e.message : String(e);
  }

  /** `connectToContainer()` and `connectToServer()`. */
  async function connect(value?: string) {
    try {
      const answer = await terminalApi.connect(kind, uuid, value);
      session?.apply(answer);
    } catch (e) {
      session?.fail(message(e));
    }
  }

  /** `loadContainers()`. */
  async function load() {
    try {
      containers = (await terminalApi.containers(kind, uuid)).containers;
      if (containers.length === 1) {
        selected = containers[0].value;
        containersLoaded = true;
        await connect(selected);
        return;
      }
      if (containers.length > 1) session?.cancelAutoStart();
    } catch (e) {
      notify.failure(message(e));
    }
    containersLoaded = true;
  }

  function selectContainer(value: string) {
    window.dispatchEvent(new CustomEvent('terminal-starting'));
    selected = value;
    targetChosen = true;
    containerOpen = false;
    connect(value);
  }

  $effect(() => chrome.listen());

  // `wire:init="loadContainers"`; a server page connects when the websocket is ready.
  $effect(() => {
    let cancelled = false;
    terminalApi.mount(kind, uuid).then((m) => {
      if (cancelled) return;
      servers = m.servers;
      mounted = true;
      if (kind !== 'server') load();
      else containersLoaded = true;
    });
    return () => {
      cancelled = true;
    };
  });

  $effect(() => {
    if (kind !== 'server' || !first?.terminal_enabled || !first.functional) return;
    let connected = false;
    const ready = () => {
      if (connected) return;
      connected = true;
      connect();
    };
    window.addEventListener('terminal-websocket-ready', ready);
    return () => window.removeEventListener('terminal-websocket-ready', ready);
  });

  $effect(() => {
    const outside = (e: MouseEvent) => {
      if (picker && !picker.contains(e.target as Node)) containerOpen = false;
    };
    window.addEventListener('click', outside);
    return () => window.removeEventListener('click', outside);
  });
</script>

{#if mounted}
  {#if consoleUnavailable}
    <section class="mt-8 w-full max-w-none xl:mt-0">
      {#if kind === 'server'}
        <Empty size="lg" title="Terminal unavailable" description="This server is not functional or terminal access is disabled." iconName="browser-terminal" />
      {:else}
        <Empty size="lg" title="Terminal unavailable" description="No containers are running, or terminal access is disabled on the destination server." iconName="browser-terminal" />
      {/if}
    </section>
  {:else}
    <section class="mt-8 mb-0! h-[calc(100dvh-8rem)] min-h-[32rem] w-full max-w-none xl:mt-0">
      <div
        data-terminal-session-canvas
        class="application-console-shell flex h-full min-h-0 flex-col overflow-hidden rounded-lg"
        data-console-theme={chrome.consoleTheme}
        style="--terminal-scrollbar: {chrome.accent}"
      >
        <header class="terminal-session-toolbar flex items-center gap-3 text-white select-none">
          {#if kind === 'server'}
            <div class="terminal-session-target-trigger flex h-8 min-w-0 max-w-sm flex-1 items-center gap-2 rounded-md px-2.5 text-xs font-medium text-white/70">
              <Reicon name="browser-terminal" class="size-3.5 shrink-0 text-white/55" />
              <span class="min-w-0 truncate font-semibold text-white/80">{first?.name}</span>
            </div>
          {:else}
            <div class="flex min-w-0 flex-1 items-center gap-2">
              {#if containers.length === 1}
                <div class="terminal-session-target-trigger flex h-8 min-w-0 max-w-sm items-center gap-2 rounded-md px-2.5 text-xs font-medium text-white/70">
                  <Reicon name="browser-terminal" class="size-3.5 shrink-0 text-white/55" />
                  <span class="min-w-0 truncate font-semibold text-white/80">{containers[0].name} · {containers[0].server_name}</span>
                </div>
              {:else if targetChosen}
                <div class="relative min-w-0" bind:this={picker}>
                  <button
                    type="button"
                    class="terminal-session-target-trigger flex h-8 max-w-[34rem] min-w-48 items-center gap-2 rounded-md px-2.5 text-xs font-medium text-white/70 transition-colors hover:bg-white/[0.08] hover:text-white"
                    onclick={() => (containerOpen = !containerOpen)}
                  >
                    <span class="min-w-0 flex-1 truncate text-left">{selectedLabel}</span>
                    <svg class="size-2.5 shrink-0 text-white/35" viewBox="0 0 12 12" fill="none" aria-hidden="true">
                      <path d="m3.5 4.75 2.5 2.5 2.5-2.5" stroke="currentColor" stroke-width="1.25" stroke-linecap="round" stroke-linejoin="round" />
                    </svg>
                  </button>
                  {#if containerOpen}
                    <div class="terminal-target-picker terminal-target-list absolute top-11 left-0 z-50 max-h-72 w-80 overflow-y-auto rounded-lg border p-1 shadow-[var(--shadow-dropdown)]">
                      {#each containers as option (option.value)}
                        <button
                          type="button"
                          class="flex h-8 w-full items-center gap-2 rounded-md px-2 text-left text-[11px] text-white/65 transition-colors hover:bg-white/[0.07] hover:text-white"
                          onclick={() => selectContainer(option.value)}
                        >
                          <span class="min-w-0 flex-1 truncate">{option.label}</span>
                          {#if option.value === selected}
                            <svg class="size-3 shrink-0 text-[#fcd452]" viewBox="0 0 12 12" fill="none" aria-hidden="true">
                              <path d="m2.5 6.25 2.1 2.1 4.9-5" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round" />
                            </svg>
                          {/if}
                        </button>
                      {/each}
                    </div>
                  {/if}
                </div>
              {/if}
            </div>
          {/if}

          <TerminalToolbarSelectors {chrome} />
        </header>

        <div class="terminal-session-panel flex min-h-0 flex-1 flex-col overflow-hidden p-3 sm:p-6">
          <div class="application-console-block min-h-0 flex-1">
            {#if kind !== 'server' && containers.length > 1 && !targetChosen}
              <div data-terminal-target-picker="launcher" class="absolute inset-0 z-20 flex items-start justify-start p-6 sm:p-10">
                <div class="terminal-target-picker w-full max-w-md rounded-lg border p-2 shadow-[var(--shadow-dropdown)]">
                  <div class="px-2 pt-1 pb-2">
                    <div class="text-sm font-semibold text-white/80">Start a terminal session</div>
                    <div class="mt-0.5 text-[11px] text-white/45">Choose a container to start a session</div>
                  </div>
                  <div class="max-h-64 overflow-y-auto">
                    {#each containers as option (option.value)}
                      <button
                        type="button"
                        class="flex h-9 w-full items-center gap-2 rounded-md px-2 text-left text-[11px] text-white/65 transition-colors hover:bg-white/[0.07] hover:text-white"
                        onclick={() => selectContainer(option.value)}
                      >
                        <Reicon name="layers" class="size-3.5 shrink-0 text-white/35" />
                        <span class="min-w-0 flex-1 truncate">{option.label}</span>
                      </button>
                    {/each}
                  </div>
                </div>
              </div>
            {/if}
            <TerminalSession bind:this={session} autoStart={kind === 'server' || !containersLoaded || containers.length === 1} />
          </div>
        </div>
      </div>
    </section>
  {/if}
{/if}
