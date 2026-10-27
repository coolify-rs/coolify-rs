<script lang="ts">
  // Port of upstream components/top-user-menu.blade.php.
  import Reicon from './Reicon.svelte';
  import { link, router } from '../lib/router.svelte';
  import { session } from '../lib/session.svelte';
  import { sessionApi } from '../lib/api';
  import { layout } from '../lib/layout.svelte';

  let { sidebar = false }: { sidebar?: boolean } = $props();

  let open = $state(false);
  let panelMaxHeight = $state<string | null>(null);
  let button: HTMLButtonElement | undefined = $state();
  let root: HTMLDivElement | undefined = $state();
  let anchor = $state({ left: 0, bottom: 0 });

  const userName = $derived(session.info?.user.name || 'Account');
  const userEmail = $derived(session.info?.user.email ?? '');
  const userInitial = $derived((session.info?.user.name || session.info?.user.email || 'A').slice(0, 1).toUpperCase());
  const collapsed = $derived(sidebar && layout.collapsed);

  function fitPanel() {
    if (!button) return;
    const rect = button.getBoundingClientRect();
    const space = sidebar ? rect.top : window.innerHeight - rect.bottom;
    panelMaxHeight = Math.max(space - 12, 160) + 'px';
    // Sidebar: fixed above the button (x-anchor.top-start.fixed.offset.4).
    anchor = { left: rect.left, bottom: window.innerHeight - rect.top + 4 };
  }

  function toggle() {
    if (open) open = false;
    else {
      fitPanel();
      open = true;
    }
  }

  async function logout() {
    open = false;
    try {
      await sessionApi.logout();
    } finally {
      session.set(null);
      router.go('/login', true);
    }
  }

  function onWindowClick(e: MouseEvent) {
    if (open && root && !root.contains(e.target as Node)) open = false;
  }
</script>

<svelte:window
  onclick={onWindowClick}
  onkeydown={(e) => e.key === 'Escape' && (open = false)}
  onresize={() => open && fitPanel()}
/>

<div class={['relative', sidebar && 'min-w-0 flex-1'].filter(Boolean).join(' ')} bind:this={root}>
  <button
    type="button"
    bind:this={button}
    onclick={toggle}
    title={userName}
    aria-label="Account menu for {userName}"
    class={['button rounded-lg px-2 normal-case', sidebar && 'min-w-0', sidebar && (collapsed ? 'w-8 px-0' : 'w-full')]
      .filter(Boolean)
      .join(' ')}
  >
    <span
      class="flex size-5 shrink-0 items-center justify-center rounded-full bg-neutral-200 text-[11px] font-semibold text-neutral-700 dark:bg-white/[0.1] dark:text-fg"
    >
      {userInitial}
    </span>
    {#if sidebar}
      <span class="min-w-0 flex-1 truncate text-left text-xs font-medium {collapsed ? 'hidden' : ''}">{userName}</span>
    {/if}
    <svg
      class="size-3.5 shrink-0 text-neutral-400 dark:text-fg-faint transition-transform {open ? 'rotate-180' : ''} {collapsed ? 'hidden' : ''}"
      viewBox="0 0 24 24"
      fill="none"
      aria-hidden="true"
    >
      <path d="M6 9l6 6 6-6" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" />
    </svg>
  </button>

  {#if open}
    <div
      style:max-height={panelMaxHeight}
      style:position={sidebar ? 'fixed' : undefined}
      style:left={sidebar ? `${anchor.left}px` : undefined}
      style:top={sidebar ? 'auto' : undefined}
      style:bottom={sidebar ? `${anchor.bottom}px` : undefined}
      class={[
        'top-user-menu-panel listbox-panel z-[90]! w-52! min-w-0! overflow-y-auto!',
        !sidebar && 'right-0! left-auto!',
        sidebar ? 'top-user-menu-panel-sidebar origin-bottom-left' : 'origin-top-right',
      ]
        .filter(Boolean)
        .join(' ')}
    >
      <div class="min-w-0 px-2 py-1.5">
        <div class="truncate text-[13px] font-semibold text-black dark:text-fg">{userName}</div>
        <div class="truncate text-[11px] text-neutral-500 dark:text-fg-faint">{userEmail}</div>
      </div>
      <div class="my-1 h-px bg-neutral-200 dark:bg-white/[0.07]"></div>

      <a href="/profile" use:link onclick={() => (open = false)} class="listbox-option">
        <span class="flex items-center gap-2">
          <Reicon name="profile" class="size-4 opacity-80" />
          Profile
        </span>
      </a>
      <a href="/profile/appearance" use:link onclick={() => (open = false)} class="listbox-option">
        <span class="flex items-center gap-2">
          <Reicon name="settings" class="size-4 opacity-80" />
          Appearance
        </span>
      </a>

      <div class="my-1 h-px bg-neutral-200 dark:bg-white/[0.07]"></div>

      <a href="https://coolify.io/docs" target="_blank" rel="noopener noreferrer" class="listbox-option">
        <span class="flex items-center gap-2">
          <Reicon name="documentation" class="size-4 opacity-80" />
          Documentation
        </span>
      </a>

      <div class="my-1 h-px bg-neutral-200 dark:bg-white/[0.07]"></div>

      <button type="button" onclick={logout} class="listbox-option w-full text-left text-error dark:text-error">
        <span class="flex items-center gap-2">
          <Reicon name="logout" class="size-4 opacity-90" />
          Log out
        </span>
      </button>
    </div>
  {/if}
</div>
