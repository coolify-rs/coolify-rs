<script lang="ts">
  // Port of upstream livewire/project/shared/terminal.blade.php with variant="application" (the only
  // one upstream renders) and `Project\Shared\Terminal`'s state. The behaviour is upstream's
  // `terminalData()` in lib/terminal/terminal.js; this file is the Alpine and Livewire side of it.
  import { onDestroy, onMount, tick, untrack } from 'svelte';
  import Empty from '../Empty.svelte';
  import { terminalData } from '../../lib/terminal/terminal.js';
  import { terminalApi, type SessionAnswer } from '../../lib/terminalApi';
  import { notify } from '../../lib/toast.svelte';

  let { autoStart = false }: { autoStart?: boolean } = $props();

  type Listener = (payload: any) => void;
  const listeners = new Map<string, Listener>();
  const watchers: ((value: boolean) => void)[] = [];

  // Upstream's `$wire`: the public properties, and what the Alpine component dispatches.
  const wire = $state({
    terminalMode: 'shell' as 'shell' | 'attach',
    attachAvailable: false,
    hasShell: true,
  });
  let isTerminalConnected = $state(false);

  const t = $state({
    ...terminalData(),
    $wire: {
      get terminalMode() {
        return wire.terminalMode;
      },
      get attachAvailable() {
        return wire.attachAvailable;
      },
      get hasShell() {
        return wire.hasShell;
      },
      on: (name: string, callback: Listener) => listeners.set(name, callback),
      dispatch(name: string, payload?: string) {
        if (name === 'error' && payload) notify.failure(payload);
        else if (name === 'terminalConnected') isTerminalConnected = true;
        else if (name === 'terminalDisconnected') isTerminalConnected = false;
      },
    },
    $refs: {} as Record<string, HTMLElement | undefined>,
    $el: undefined as HTMLElement | undefined,
    $nextTick: (callback: () => void) => {
      tick().then(callback);
    },
    $watch: (_name: string, callback: (value: boolean) => void) => watchers.push(callback),
  });

  // `$dispatch` of the session events, for the page around this component.
  export function apply(answer: SessionAnswer): void {
    if (answer.has_shell !== undefined) wire.hasShell = answer.has_shell;
    if (answer.attach_available !== undefined) wire.attachAvailable = answer.attach_available;
    if (answer.mode !== undefined) wire.terminalMode = answer.mode;
    if (answer.ok && answer.token) {
      listeners.get('send-terminal-token')?.([answer.token]);
      listeners.get('terminal-should-focus')?.(undefined);
    } else {
      listeners.get('terminal-session-failed')?.({ message: answer.message });
    }
  }

  export function fail(message: string): void {
    listeners.get('terminal-session-failed')?.({ message });
  }

  export function cancelAutoStart(): void {
    listeners.get('terminal-auto-start-cancelled')?.(undefined);
  }

  let wrapper = $state<HTMLElement>();
  let consoleInput = $state<HTMLElement>();
  let container = $state<HTMLElement>();
  let watching = false;

  $effect(() => {
    t.$refs.consoleInput = consoleInput;
  });

  $effect(() => {
    const active: boolean = t.terminalActive;
    if (!watching) {
      watching = true;
      return;
    }
    untrack(() => watchers.forEach((w) => w(active)));
  });

  // `wire:poll.keep-alive.30s="keepTerminalPageAlive"`.
  $effect(() => {
    if (!isTerminalConnected) return;
    const poll = setInterval(() => terminalApi.keepAlive().catch(() => {}), 30000);
    return () => clearInterval(poll);
  });

  const starting = () => {
    t.beginTerminalSessionStart();
    t.setTerminalTheme(localStorage.getItem('coolify-console-theme') ?? 'system');
  };
  const themeChange = (e: Event) => t.setTerminalTheme((e as CustomEvent).detail.theme);

  onMount(() => {
    t.$refs.terminalWrapper = wrapper;
    t.$refs.consoleInput = consoleInput;
    t.$el = container;
    if (container) container.dataset.autoStart = autoStart ? 'true' : 'false';
    window.addEventListener('terminal-starting', starting);
    window.addEventListener('terminal-theme-change', themeChange);
    t.init();
  });

  onDestroy(() => {
    window.removeEventListener('terminal-starting', starting);
    window.removeEventListener('terminal-theme-change', themeChange);
    t.cleanup();
    t.destroy();
  });

  const sessionLabel = $derived(
    t.connectionState === 'reconnecting'
      ? `reconnecting… (attempt ${t.reconnectAttempts})`
      : t.starting || t.connectionState === 'connecting'
        ? 'connecting…'
        : 'choose a container to start a session',
  );
</script>

<div id="terminal-container" bind:this={container} class="group/terminal relative h-full min-h-0 bg-transparent">
  {#if !wire.hasShell && wire.terminalMode === 'shell'}
    <div class="flex h-full min-h-[32rem] items-center justify-center">
      <Empty size="lg" title="Shell unavailable" description="This container does not include Bash or sh. Install a supported shell to use the terminal." iconName="browser-terminal" />
    </div>
  {/if}

  <div
    bind:this={wrapper}
    data-console-theme={t.fullscreen ? t.selectedTheme : null}
    class={t.fullscreen
      ? 'terminal-fullscreen-shell fixed inset-0 z-[100000] m-0 flex w-screen max-w-none flex-col overflow-hidden rounded-none p-0'
      : 'relative flex h-full min-h-0 w-full flex-col overflow-hidden bg-transparent'}
  >
    {#if !t.terminalActive}
      <div class="pointer-events-none absolute inset-0 z-10 flex items-center justify-center bg-transparent">
        {#if t.connectionError}
          <div data-terminal-connection-error role="alert" class="terminal-loading-label pointer-events-auto flex max-w-md flex-col items-center gap-3 px-4 text-center">
            <span>{t.connectionError}</span>
            <button type="button" onclick={() => t.reloadTerminalPage()} class="cursor-pointer rounded-md border border-current/30 px-3 py-1 text-xs font-medium transition-colors hover:bg-current/10">Reload page</button>
          </div>
        {:else}
          <div class="terminal-loading-label flex items-center gap-2">
            {#if t.starting || t.connectionState === 'connecting' || t.connectionState === 'reconnecting'}
              <svg class="size-3 animate-spin" viewBox="0 0 24 24" fill="none">
                <circle class="opacity-25" cx="12" cy="12" r="9" stroke="currentColor" stroke-width="3" />
                <path class="opacity-75" d="M21 12a9 9 0 0 0-9-9" stroke="currentColor" stroke-width="3" stroke-linecap="round" />
              </svg>
            {/if}
            <span>{sessionLabel}</span>
          </div>
        {/if}
      </div>
    {/if}

    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div
      id="terminal"
      onmouseup={() => t.focusConsoleFromOutput()}
      data-terminal-style="application"
      class={t.fullscreen
        ? 'terminal-host relative z-[1] min-h-0 flex-1 overflow-hidden px-1 py-[5px] bg-transparent'
        : 'terminal-host relative min-h-0 flex-1 overflow-hidden pt-[5px] pr-px pb-[5px] pl-1 bg-transparent'}
    ></div>

    <!-- Command box for console (attach) mode: a terminal input line, never raw keystrokes. -->
    <div
      class="terminal-console-input relative z-[2] mt-2 shrink-0 border-t pt-2 {t.fullscreen ? 'mx-1 mb-1' : ''}"
      style:display={t.terminalActive && wire.terminalMode === 'attach' ? null : 'none'}
      style="--console-fg: {t.consoleForegroundColor}"
    >
      {#if t.consolePendingLines.length > 0}
        <div class="console-command-pending mb-1 flex items-center gap-2 pl-2 text-xs">
          <span>{t.consolePendingLines.length} pasted lines ready to send</span>
          <span class="opacity-60">· Enter to send · Esc to cancel</span>
        </div>
      {/if}
      <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
      <div class="console-command-row flex min-h-9 cursor-text items-center gap-3 rounded-md pr-1 pl-2" onclick={() => consoleInput?.focus()}>
        <span class="console-command-prompt shrink-0 font-mono text-sm leading-none select-none" data-sent={t.consoleSentFlash ? '' : null} aria-hidden="true">&gt;</span>
        <input
          type="text"
          bind:this={consoleInput}
          bind:value={t.consoleCommand}
          onkeydown={(e) => {
            if (e.key === 'Enter') {
              e.preventDefault();
              t.submitConsoleCommand();
            } else if (e.key === 'Escape') {
              e.preventDefault();
              e.stopPropagation();
              t.cancelConsoleInput();
            } else if (e.ctrlKey && e.key === 'l') {
              e.preventDefault();
              t.clearConsoleOutput();
            } else if (e.key === 'ArrowUp') {
              e.preventDefault();
              t.recallConsoleCommand(-1);
            } else if (e.key === 'ArrowDown') {
              e.preventDefault();
              t.recallConsoleCommand(1);
            }
          }}
          onpaste={(e) => t.pasteConsoleText(e)}
          autocomplete="off"
          autocapitalize="off"
          spellcheck="false"
          enterkeyhint="send"
          aria-label="Console command"
          placeholder="Send a command to the process…"
          class="console-command-field min-w-0 flex-1 border-0! bg-transparent! px-0! py-1.5! font-mono text-[16px]! shadow-none! ring-0! outline-none! focus:border-0! focus:shadow-none! focus:ring-0! focus:outline-none! sm:text-[13px]!"
        />
        <button
          type="button"
          onclick={(e) => {
            e.stopPropagation();
            t.submitConsoleCommand();
          }}
          disabled={!t.canSubmitConsoleCommand()}
          title="Send (Enter) · Clear input (Esc) · Clear output (Ctrl+L)"
          class="console-command-send inline-flex h-7 shrink-0 items-center gap-2 rounded px-2 text-xs font-medium transition-colors focus-visible:outline-2 focus-visible:outline-offset-2 disabled:pointer-events-none disabled:opacity-40"
        >
          Send
          <span class="font-mono text-sm opacity-60" aria-hidden="true">↵</span>
        </button>
      </div>
    </div>

    {#if t.terminalActive && wire.terminalMode !== 'attach'}
      <div
        class="sm:hidden {t.fullscreen ? 'relative z-[2] shrink-0 px-2 pb-2' : t.keyboardInset > 0 ? 'fixed inset-x-0 z-[100002] px-2 pb-2' : 'relative z-[2] mt-2 shrink-0'}"
        style={!t.fullscreen && t.keyboardInset > 0 ? `top: ${t.keyboardAnchorTop}px; transform: translateY(-100%)` : ''}
        data-terminal-mobile-toolbar
      >
        <div class="terminal-key-row mx-auto flex max-w-3xl gap-1.5 overflow-x-auto whitespace-nowrap rounded-lg px-2 py-1.5 text-white [scrollbar-width:thin]">
          <button type="button" class="terminal-mobile-key" onclick={() => t.pasteFromClipboard()}>paste</button>
          <button type="button" class="terminal-mobile-key" onclick={() => t.copyTerminalSelection()}>copy</button>
          <button type="button" class="terminal-mobile-key" onclick={() => t.sendTerminalControl('escape')}>ESC</button>
          <button type="button" class="terminal-mobile-key" onclick={() => t.sendTerminalControl('tab')}>tab</button>
          <button type="button" class="terminal-mobile-key {t.terminalModifier === 'ctrl' ? 'border-white/35 bg-white/20 text-white' : ''}" onclick={() => t.toggleTerminalModifier('ctrl')}>ctrl</button>
          <button type="button" class="terminal-mobile-key {t.terminalModifier === 'alt' ? 'border-white/35 bg-white/20 text-white' : ''}" onclick={() => t.toggleTerminalModifier('alt')}>alt</button>
          <button type="button" class="terminal-mobile-key" onclick={() => t.sendTerminalKey('/')}>/</button>
          <button type="button" class="terminal-mobile-key" onclick={() => t.sendTerminalKey('|')}>|</button>
          <button type="button" class="terminal-mobile-key" onclick={() => t.sendTerminalKey('~')}>~</button>
          <button type="button" class="terminal-mobile-key" onclick={() => t.sendTerminalKey('-')}>-</button>
          <button type="button" class="terminal-mobile-key" onclick={() => t.sendTerminalControl('ctrlC')}>^C</button>
          <button type="button" class="terminal-mobile-key" onclick={() => t.sendTerminalControl('ctrlD')}>^D</button>
          <button type="button" class="terminal-mobile-key" onclick={() => t.sendTerminalControl('ctrlBackslash')}>^\</button>
          <button type="button" class="terminal-mobile-key" onclick={() => t.sendTerminalControl('ctrlS')}>^S</button>
          <button type="button" class="terminal-mobile-key" onclick={() => t.sendTerminalControl('ctrlZ')}>^Z</button>
        </div>
      </div>
    {/if}

    <!-- Enter and exit use identical chrome so the toggle does not jump size or gain or lose a box. -->
    {#if t.fullscreen}
      <button type="button" title="Exit fullscreen" class="terminal-fullscreen-btn fixed top-3 right-3 z-[100001]" onclick={() => t.makeFullscreen()}>
        <svg class="size-3.5" viewBox="0 0 24 24" fill="none" aria-hidden="true">
          <path d="M9 4v5H4M15 20v-5h5M20 9h-5V4M4 15h5v5" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" />
        </svg>
      </button>
    {:else if t.terminalActive}
      <button type="button" title="Fullscreen" class="terminal-fullscreen-btn absolute z-20 right-2 top-2 opacity-100 sm:opacity-0 sm:group-hover/terminal:opacity-100 sm:focus-visible:opacity-100" onclick={() => t.makeFullscreen()}>
        <svg class="size-3.5" viewBox="0 0 24 24" fill="none" aria-hidden="true">
          <path d="M9 4H4v5M15 4h5v5M20 15v5h-5M4 15v5h5" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" />
        </svg>
      </button>
    {/if}
  </div>
</div>
