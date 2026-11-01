<script lang="ts">
  import DeploymentsIndicator from './DeploymentsIndicator.svelte';
  // Port of upstream layouts/app.blade.php: desktop top bar and sidebar,
  // mobile top bar with a slide-over sidebar, and the main column. The brand
  // cell carries the coolify-rs wordmark and logo.
  import type { Snippet } from 'svelte';
  import Navbar from './Navbar.svelte';
  import Breadcrumb from './Breadcrumb.svelte';
  import TeamSwitcher from './TeamSwitcher.svelte';
  import UserMenu from './UserMenu.svelte';
  import ServerTopbar from './ServerTopbar.svelte';
  import UpgradeBadge from './UpgradeBadge.svelte';
  import LayoutPopups from './LayoutPopups.svelte';
  import GlobalSearch from './GlobalSearch.svelte';
  import { page } from '../lib/page.svelte';
  import { layout } from '../lib/layout.svelte';
  import { link, router } from '../lib/router.svelte';
  import { session } from '../lib/session.svelte';

  let { children, fullBleed = false }: { children: Snippet; fullBleed?: boolean } = $props();

  const version = $derived(session.info?.version ?? '');
  const collapsed = $derived(layout.collapsed);

  // The sidebar animates its width only after the first paint.
  let sidebarReady = $state(false);
  $effect(() => {
    requestAnimationFrame(() => (sidebarReady = true));
  });

  // Close the mobile sheet on navigation.
  $effect(() => {
    void router.path;
    layout.mobileOpen = false;
  });

  function markScrolled() {
    document.documentElement.classList.toggle('is-scrolled', window.scrollY > 2);
  }
  $effect(markScrolled);
</script>

<svelte:window onscroll={markScrolled} onkeydown={(e) => e.key === 'Escape' && (layout.mobileOpen = false)} />

<LayoutPopups />
<GlobalSearch />
<div class="app-shell dark:text-inherit text-black {fullBleed ? 'app-shell-canvas flex h-dvh flex-col' : ''}" style="--sidebar-w: {collapsed ? 'var(--sidebar-collapsed-w)' : '14rem'}">
  <!-- Desktop top bar -->
  <header class="app-topbar hidden lg:flex fixed top-0 inset-x-0 z-50 h-12 items-center bg-white/95 dark:bg-panel/95 backdrop-blur">
    <div
      class="app-topbar-brand flex items-center gap-2 h-full shrink-0 border-r border-black/[0.06] bg-neutral-50 dark:border-white/[0.04] dark:bg-app transition-[width] duration-200 {collapsed
        ? 'w-(--sidebar-collapsed-w) justify-center px-0 border-b'
        : 'w-56 px-4'}"
    >
      <div class="flex items-baseline gap-1.5 min-w-0">
        <a href="/" use:link title="coolify-rs" class="flex shrink-0 items-center hover:opacity-80 transition-opacity">
          {#if collapsed}
            <img src="/coolify-rs-logo.svg" alt="coolify-rs" class="size-5" />
          {:else}
            <span class="text-[15px] font-semibold tracking-tight text-black dark:text-white">coolify-rs</span>
          {/if}
        </a>
        {#if !collapsed}
          <span
            class="text-xs opacity-90 min-w-0 truncate !text-[10.5px] font-medium text-neutral-400 dark:text-fg-faint !opacity-100 hover:!opacity-100 dark:hover:text-fg hover:text-black"
            title="v{version}">v{version}</span
          >
        {/if}
      </div>
      {#if session.isInstanceAdmin && !collapsed}
        <div class="ml-auto shrink-0"><UpgradeBadge /></div>
      {/if}
    </div>
    <div class="flex h-full items-center gap-0.5 min-w-0 flex-1 border-b border-black/[0.06] pl-3 pr-4 dark:border-white/[0.04]">
      <div class="relative flex min-w-0 flex-1 items-center">
        <Breadcrumb />
        <div id="server-topbar-context" class="min-w-0">
          {#if page.server}<ServerTopbar server={page.server} />{/if}
        </div>
      </div>
      <!-- Resource pages teleport into these (layouts/app.blade.php). -->
      <div id="page-toolbar-slot" class="flex shrink-0 items-center empty:hidden"></div>
      <div id="configuration-warning-hud-slot" class="relative shrink-0"></div>
      <DeploymentsIndicator variant="topbar" />
      <div id="resource-action-hud-slot" class="hidden shrink-0 items-center xl:flex"></div>
    </div>
  </header>

  <!-- Mobile slide-over sidebar -->
  <div class="mobile-sidebar-sheet relative z-[1000] lg:hidden">
    {#if layout.mobileOpen}
      <div class="fixed inset-0 bg-black/50" role="presentation" onclick={() => (layout.mobileOpen = false)}></div>
    {/if}
    <div class="fixed inset-y-0 right-0 flex {layout.mobileOpen ? '' : 'pointer-events-none'}">
      {#if layout.mobileOpen}
        <div
          role="dialog"
          aria-modal="true"
          aria-label="Navigation menu"
          class="relative flex h-full w-72 max-w-[85vw] min-w-0 flex-col overflow-hidden rounded-l-2xl border-l border-neutral-200 bg-white shadow-[-8px_0_30px_-6px_rgba(0,0,0,0.18)] dark:border-white/[0.12] dark:bg-panel dark:shadow-[-8px_0_30px_-4px_rgba(0,0,0,0.5)]"
        >
          <div class="flex h-12 shrink-0 items-center justify-between gap-1.5 border-b border-neutral-200 px-4 dark:border-white/[0.06]">
            <div class="flex min-w-0 items-baseline gap-1.5">
              <a
                href="/"
                use:link
                title="coolify-rs"
                class="text-[15px] font-semibold tracking-tight text-black transition-opacity hover:opacity-80 dark:text-white"
              >
                coolify-rs
              </a>
              <span
                class="text-xs opacity-90 !text-[10.5px] font-medium text-neutral-400 dark:text-fg-faint !opacity-100 hover:!opacity-100 hover:text-black dark:hover:text-fg"
                >v{version}</span
              >
            </div>
            <button
              type="button"
              onclick={() => (layout.mobileOpen = false)}
              aria-label="Close menu"
              class="-mr-1.5 flex size-8 shrink-0 items-center justify-center rounded-md text-neutral-500 transition-colors hover:bg-neutral-100 hover:text-black active:scale-95 dark:text-fg-dim dark:hover:bg-white/[0.06] dark:hover:text-fg"
            >
              <svg class="size-5" fill="none" viewBox="0 0 24 24" stroke-width="1.75" stroke="currentColor" aria-hidden="true">
                <path stroke-linecap="round" stroke-linejoin="round" d="M6 18L18 6M6 6l12 12" />
              </svg>
            </button>
          </div>
          <div class="flex min-h-0 min-w-0 flex-1 flex-col overflow-y-auto pb-2 scrollbar">
            <Navbar />
          </div>
        </div>
      {/if}
    </div>
  </div>

  <!-- Desktop sidebar (below the top bar) -->
  <div
    class="app-sidebar hidden lg:fixed lg:top-12 lg:bottom-0 lg:left-0 lg:z-40 lg:flex lg:flex-col min-w-0 {collapsed
      ? 'lg:w-(--sidebar-collapsed-w)'
      : 'lg:w-56'} {sidebarReady ? 'transition-[width] duration-200' : ''}"
  >
    <div class="flex min-h-0 grow min-w-0 flex-col overflow-visible">
      <Navbar desktop />
    </div>
  </div>

  <!-- Mobile top bar -->
  <div
    class="sticky top-0 z-40 flex items-center justify-between px-4 py-3 gap-x-4 sm:px-6 lg:hidden bg-white/95 dark:bg-panel/95 backdrop-blur-sm border-b border-neutral-200/60 dark:border-white/[0.06]"
  >
    <div class="flex min-w-0 flex-1 items-center gap-2.5">
      <a
        href="/"
        use:link
        class="flex size-8 shrink-0 items-center justify-center rounded-lg bg-neutral-100 transition-opacity hover:opacity-80 dark:bg-white/[0.06]"
      >
        <img src="/coolify-rs-logo.svg" alt="coolify-rs" class="w-[18px] h-[18px]" />
      </a>
      <div class="min-w-0">
        <TeamSwitcher />
      </div>
    </div>
    <div class="flex shrink-0 items-center gap-1">
      <div id="configuration-warning-hud-slot-mobile" class="relative shrink-0"></div>
      <DeploymentsIndicator variant="mobile" />
      {#if session.isInstanceAdmin}<UpgradeBadge />{/if}
      <UserMenu />
      <button
        type="button"
        onclick={() => (layout.mobileOpen = !layout.mobileOpen)}
        class="-mr-1 flex size-9 items-center justify-center rounded-md text-neutral-500 transition-transform duration-100 ease-out hover:bg-neutral-100 hover:text-black active:scale-90 dark:text-fg-dim dark:hover:bg-white/[0.06] dark:hover:text-fg"
      >
        <span class="sr-only">Open sidebar</span>
        <svg class="size-5" viewBox="0 0 24 24" fill="none" aria-hidden="true">
          <rect x="3" y="5" width="18" height="14" rx="4" stroke="currentColor" stroke-width="1.6" />
          <path d="M16 9v6" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" />
        </svg>
      </button>
    </div>
  </div>

  {#if fullBleed}
    <!-- Full-bleed pages (canvases) fill the space between the top bar and the sidebar. -->
    <main
      class="app-main app-main-full relative min-h-0 flex-1 bg-neutral-50 dark:bg-app lg:pt-12 {collapsed ? 'lg:ml-(--sidebar-collapsed-w)' : 'lg:ml-56'} {sidebarReady
        ? 'transition-[margin] duration-200'
        : ''}"
    >
      <div class="relative h-full">
        {@render children()}
      </div>
    </main>
  {:else}
  <main
    class="app-main min-h-screen bg-neutral-50 dark:bg-app px-5 py-6 sm:px-8 lg:px-10 lg:pt-[calc(3rem+1.75rem)] lg:pb-10 {collapsed
      ? 'lg:ml-(--sidebar-collapsed-w)'
      : 'lg:ml-56'} {sidebarReady ? 'transition-[margin] duration-200' : ''} {layout.pageWidth === 'centered' ? '' : 'app-main-wide'}"
  >
    <div class="w-full {layout.pageWidth === 'centered' ? 'mx-auto max-w-[1400px]' : 'max-w-none'}">
      {#if session.info?.features.read_only}
        <div class="mb-4 rounded-lg border border-warning/30 bg-warning/10 px-4 py-3 text-sm text-warning">
          coolify-rs is in read-only mode: the database schema does not match this build. Changes are disabled.
        </div>
      {/if}
      {@render children()}
    </div>
  </main>
  {/if}
</div>
