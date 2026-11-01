<script lang="ts">
  // Port of upstream components/terminal/mode-selector.blade.php and theme-selector.blade.php.
  import { consoleThemes, type ConsoleChrome } from '../../lib/terminal/chrome.svelte';

  let { chrome }: { chrome: ConsoleChrome } = $props();

  const modes = [
    { key: 'shell', name: 'Shell', description: 'Interactive shell (docker exec)' },
    { key: 'attach', name: 'Console', description: 'Attach to the app process (docker attach)' },
  ] as const;

  let modeBox = $state<HTMLElement>();
  let themeBox = $state<HTMLElement>();

  $effect(() => {
    const outside = (e: MouseEvent) => {
      if (modeBox && !modeBox.contains(e.target as Node)) chrome.modeOpen = false;
      if (themeBox && !themeBox.contains(e.target as Node)) chrome.themeOpen = false;
    };
    const escape = (e: KeyboardEvent) => {
      if (e.key === 'Escape') chrome.modeOpen = false;
    };
    window.addEventListener('click', outside);
    window.addEventListener('keydown', escape);
    return () => {
      window.removeEventListener('click', outside);
      window.removeEventListener('keydown', escape);
    };
  });
</script>

<div class="ml-auto flex items-center gap-2">
  {#if chrome.attachAvailable}
    <div class="relative shrink-0" bind:this={modeBox}>
      <button
        type="button"
        class="terminal-theme-trigger flex h-8 items-center gap-1.5 rounded-md px-2.5 text-xs font-medium text-white/70 transition-colors hover:bg-white/[0.08] hover:text-white"
        onclick={() => (chrome.modeOpen = !chrome.modeOpen)}
        aria-label="Choose terminal mode"
        aria-haspopup="menu"
        aria-expanded={chrome.modeOpen}
      >
        <svg class="size-3.5 text-white/45" viewBox="0 0 24 24" fill="none" aria-hidden="true">
          <path d="m5 8 3 3-3 3M12 16h6" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" />
        </svg>
        <span>{modes.find((m) => m.key === chrome.terminalMode)?.name}</span>
        <svg class="size-2.5 text-white/35" viewBox="0 0 12 12" fill="none" aria-hidden="true">
          <path d="m3.5 4.75 2.5 2.5 2.5-2.5" stroke="currentColor" stroke-width="1.25" stroke-linecap="round" stroke-linejoin="round" />
        </svg>
      </button>

      {#if chrome.modeOpen}
        <div
          role="menu"
          class="console-theme-selector absolute top-11 right-0 z-50 w-64 overflow-y-auto rounded-lg border border-neutral-200 bg-white p-1 shadow-[0_18px_50px_rgba(0,0,0,0.18)] dark:border-white/[0.1] dark:bg-[#111113] dark:shadow-[0_18px_50px_rgba(0,0,0,0.55)]"
        >
          {#each modes as mode (mode.key)}
            {#if mode.key !== 'shell' || chrome.hasShell}
              <button
                type="button"
                role="menuitemradio"
                aria-checked={chrome.terminalMode === mode.key}
                class="flex w-full items-start gap-2 rounded-md px-2 py-1.5 text-left text-neutral-600 transition-colors hover:bg-neutral-100 hover:text-neutral-950 dark:text-white/65 dark:hover:bg-white/[0.07] dark:hover:text-white"
                onclick={() => chrome.setTerminalMode(mode.key)}
              >
                <span class="min-w-0 flex-1">
                  <span class="block text-[12px] font-medium">{mode.name}</span>
                  <span class="mt-0.5 block text-[10px] text-neutral-400 dark:text-white/35">{mode.description}</span>
                </span>
                {#if chrome.terminalMode === mode.key}
                  <svg class="mt-0.5 size-3 shrink-0 text-[#fcd452]" viewBox="0 0 12 12" fill="none" aria-hidden="true">
                    <path d="m2.5 6.25 2.1 2.1 4.9-5" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round" />
                  </svg>
                {/if}
              </button>
            {/if}
          {/each}
        </div>
      {/if}
    </div>
  {/if}

  <div class="relative ml-auto shrink-0" bind:this={themeBox}>
    <button
      type="button"
      class="terminal-theme-trigger flex h-8 items-center gap-2 rounded-md px-2.5 text-xs font-medium text-white/70 transition-colors hover:bg-white/[0.08] hover:text-white"
      onclick={() => (chrome.themeOpen = !chrome.themeOpen)}
      aria-label="Choose terminal theme"
      aria-expanded={chrome.themeOpen}
    >
      <span class="size-2 rounded-full ring-1 ring-white/20" style:background-color={chrome.accent}></span>
      <span>{chrome.themeName}</span>
      <svg class="size-2.5 text-white/35" viewBox="0 0 12 12" fill="none" aria-hidden="true">
        <path d="m3.5 4.75 2.5 2.5 2.5-2.5" stroke="currentColor" stroke-width="1.25" stroke-linecap="round" stroke-linejoin="round" />
      </svg>
    </button>

    {#if chrome.themeOpen}
      <div class="console-theme-selector absolute top-11 right-0 z-50 max-h-80 w-56 overflow-y-auto rounded-lg border border-neutral-200 bg-white p-1 shadow-[var(--shadow-dropdown)] dark:border-white/[0.1] dark:bg-[#111113]">
        {#each consoleThemes as theme (theme.key)}
          <button
            type="button"
            class="flex h-8 w-full items-center gap-2 rounded-md px-2 text-left text-[11px] text-neutral-600 transition-colors hover:bg-neutral-100 hover:text-neutral-950 dark:text-white/65 dark:hover:bg-white/[0.07] dark:hover:text-white"
            onclick={() => chrome.setTheme(theme.key)}
          >
            <span class="h-3 w-5 rounded-full border border-white/10" style="background: {theme.background}"></span>
            <span class="flex-1">{theme.name}</span>
            {#if chrome.consoleTheme === theme.key}
              <svg class="size-3 text-[#fcd452]" viewBox="0 0 12 12" fill="none" aria-hidden="true">
                <path d="m2.5 6.25 2.1 2.1 4.9-5" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round" />
              </svg>
            {/if}
          </button>
        {/each}
      </div>
    {/if}
  </div>
</div>
