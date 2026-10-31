<script lang="ts">
  // Port of upstream components/navbar.blade.php: the main menu, with parent
  // entries opening their submenu in place (and a back row to return).
  import Reicon from './Reicon.svelte';
  import UserMenu from './UserMenu.svelte';
  import { visibleSections, isParent, pathIs, type NavItem, type NavLeaf } from '../lib/nav';
  import { router, link } from '../lib/router.svelte';
  import { session } from '../lib/session.svelte';
  import { layout } from '../lib/layout.svelte';
  import { page } from '../lib/page.svelte';
  import BetaBadge from './BetaBadge.svelte';

  let { desktop = false }: { desktop?: boolean } = $props();

  const sections = $derived(
    visibleSections({ isAdmin: session.isAdmin, isOwner: session.isOwner, isInstanceAdmin: session.isInstanceAdmin }),
  );
  const leafActive = (l: NavLeaf) => pathIs(router.path, l.active);
  const itemActive = (i: NavItem) => (isParent(i) ? i.children.some(leafActive) : leafActive(i));
  const submenus = $derived(sections.flatMap(([, items]) => items).filter(isParent));
  const activeSubmenu = $derived(submenus.find(itemActive)?.label ?? '');

  // The submenu of the current page opens on every navigation, as upstream's re-rendered navbar does.
  let submenu = $state('');
  let navSwap = $state('');
  // -1: main menu or a submenu; 0: the page's resource menu (starts open); 1: its parent menu.
  let navLevel = $state(-1);
  $effect.pre(() => {
    submenu = activeSubmenu;
    navSwap = '';
  });
  $effect.pre(() => {
    navLevel = page.navContext ? 0 : -1;
  });
  const shownContext = $derived(navLevel === 0 ? page.navContext : navLevel === 1 ? (page.navContext?.parent ?? null) : null);
  const showContext = $derived(!!shownContext);

  const collapsed = $derived(layout.collapsed);
  const centerWhenCollapsed = $derived(collapsed ? 'lg:justify-center lg:px-0' : '');
  const hideWhenCollapsed = $derived(collapsed ? 'lg:hidden' : '');

  // Collapsed sidebar: show the item name in a tooltip next to the hovered item.
  let tooltip = $state({ text: '', x: 0, y: 0, show: false });
  function onMouseOver(e: MouseEvent) {
    if (!collapsed) return;
    const el = (e.target as HTMLElement).closest<HTMLElement>('.menu-item, .menu-subitem');
    if (!el) {
      tooltip.show = false;
      return;
    }
    const text = el.getAttribute('title') || el.getAttribute('aria-label') || el.querySelector('.menu-item-label')?.textContent?.trim() || '';
    if (!text) return;
    const rect = el.getBoundingClientRect();
    tooltip = { text, x: rect.right + 8, y: rect.top + rect.height / 2, show: true };
  }

  function openSearch() {
    window.dispatchEvent(new CustomEvent('open-global-search'));
  }

  const modKeyLabel = /Mac|iPhone|iPad/.test(navigator.platform) ? '⌘' : 'Ctrl+';
  const navClass = (l: NavLeaf | NavItem) => ['menu-item', itemActive(l) && 'menu-item-active', centerWhenCollapsed].filter(Boolean).join(' ');

  // `window.scrollToSettingsSection` (resources/js/app.js): scroll, then a short highlight.
  let activeSection = $state('');
  function scrollToSection(id: string) {
    activeSection = id;
    const el = document.getElementById(id);
    if (!el) return;
    el.scrollIntoView({ behavior: 'smooth', block: 'start' });
    el.classList.remove('is-section-highlight');
    void el.offsetWidth;
    el.classList.add('is-section-highlight');
    setTimeout(() => el.classList.remove('is-section-highlight'), 500);
  }
</script>

<nav
  class={[
    'sidebar-nav flex min-h-0 flex-col flex-1 border-r border-black/[0.06] dark:border-white/[0.04] pt-2',
    desktop ? 'bg-neutral-50 dark:bg-app' : 'bg-white dark:bg-panel',
    collapsed ? 'px-2 lg:px-3 sidebar-collapsed' : 'px-2 lg:px-3',
  ].join(' ')}
  onmouseover={onMouseOver}
  onfocus={() => {}}
  onmouseleave={() => (tooltip.show = false)}
>
  <div class={['px-1 pb-3', collapsed && 'lg:px-0'].filter(Boolean).join(' ')}>
    <button
      onclick={openSearch}
      type="button"
      title={`Search (Press / or ${modKeyLabel}K)`}
      class="menu-item menu-search justify-between !rounded-lg !bg-neutral-100 dark:!bg-white/[0.04] hover:!bg-neutral-200 dark:hover:!bg-white/[0.07] !text-fg-faint"
    >
      <span class="flex items-center gap-2.5 min-w-0">
        <Reicon name="search" class="menu-item-icon" />
        <span class="menu-item-label truncate {hideWhenCollapsed}">Search</span>
      </span>
      <kbd
        class="px-1.5 py-0.5 text-[11px] font-medium text-fg-faint bg-neutral-200 dark:bg-white/[0.06] rounded-lg border border-transparent dark:border-white/5 {hideWhenCollapsed}"
        >{modKeyLabel}K</kbd
      >
    </button>
  </div>

  <ul role="list" class="sidebar-menu -mx-1 flex min-h-0 flex-1 flex-col gap-y-0.5 overflow-y-auto px-1 pb-2">
    {#if showContext && shownContext}
      {@const ctx = shownContext}
      <li class="contents">
        <ul role="list" class="flex flex-col gap-y-0.5 {navSwap && 'nav-swap-' + navSwap}">
          <li class="mb-1">
            <button
              type="button"
              onclick={() => {
                navLevel = navLevel === 0 && ctx.parent ? 1 : -1;
                navSwap = 'back';
              }}
              title={ctx.backTitle}
              aria-label={ctx.backTitle}
              class="menu-item menu-back bg-transparent! hover:bg-neutral-100! dark:hover:bg-white/[0.06]! {centerWhenCollapsed}"
            >
              <span class="menu-back-icon flex shrink-0 items-center justify-center">
                <Reicon name="chevron-down" class="size-3 rotate-90 text-coollabs dark:text-fg-dim" />
              </span>
              <span class="flex-1 truncate pr-6 text-center font-medium text-black dark:text-fg {hideWhenCollapsed}">{ctx.label}</span>
            </button>
          </li>
          <li class="nav-context min-w-0">
            <div class="nav-context-menu">
              <nav aria-label={ctx.ariaLabel} class="grid grid-cols-1 gap-0.5">
                {#each ctx.groups as group, gi (gi)}
                  {#if gi > 0}
                    <div class="my-2 border-t border-neutral-200 dark:border-white/[0.06]" aria-hidden="true"></div>
                  {/if}
                  {#each group as item (item.label)}
                    {#if item.disabled}
                      <span class="menu-item cursor-not-allowed opacity-40" aria-disabled="true" title="Validate the server connection first.">
                        <Reicon name={item.icon} class="menu-item-icon" />
                        <span class="menu-item-label">{item.label}</span>
                      </span>
                    {:else}
                      <a class="menu-item {item.active ? 'menu-item-active' : ''}" use:link href={item.href}>
                        <Reicon name={item.icon} class="menu-item-icon" />
                        <span class="menu-item-label">{item.label}</span>
                        {#if item.warning}<Reicon name="alert-triangle" class="ml-auto size-3.5 shrink-0 text-orange-500 dark:text-warning" />{:else if item.beta}<BetaBadge class="ml-auto shrink-0" />{/if}
                      </a>
                      {#if item.active && item.links?.length}
                        <div class="col-span-full grid grid-cols-1 gap-0.5 border-l border-neutral-200 pl-2 dark:border-white/[0.08]">
                          {#each item.links as child (child.href)}
                            <a class="menu-item {child.active ? 'menu-item-active' : ''}" use:link href={child.href}>
                              <Reicon name={child.icon} class="menu-item-icon" />
                              <span class="menu-item-label">{child.label}</span>
                              {#if child.beta}<BetaBadge class="ml-auto shrink-0" />{/if}
                            </a>
                          {/each}
                        </div>
                      {/if}
                      {#if item.active && item.children?.length}
                        <div class="nav-children flex flex-col gap-0.5 py-1">
                          {#each item.children as child (child.id)}
                            <button
                              type="button"
                              class="menu-subitem {activeSection === child.id ? 'menu-subitem-active' : ''}"
                              onclick={() => scrollToSection(child.id)}
                            >
                              <span class="menu-item-label text-left">{child.label}</span>
                            </button>
                          {/each}
                        </div>
                      {/if}
                    {/if}
                  {/each}
                {/each}
              </nav>
            </div>
          </li>
        </ul>
      </li>
    {/if}
    {#each submenus as parent (parent.label)}
      {#if !showContext && submenu === parent.label}
        <li class="contents">
          <ul role="list" class="flex flex-col gap-y-0.5 {navSwap && 'nav-swap-' + navSwap}">
            <li class="mb-1">
              <button
                type="button"
                onclick={() => {
                  submenu = '';
                  navSwap = 'back';
                }}
                title="Back to the main menu"
                aria-label="Back to the main menu"
                class="menu-item menu-back bg-transparent! hover:bg-neutral-100! dark:hover:bg-white/[0.06]! {centerWhenCollapsed}"
              >
                <span class="menu-back-icon flex shrink-0 items-center justify-center">
                  <Reicon name="chevron-down" class="size-3 rotate-90 text-coollabs dark:text-fg-dim" />
                </span>
                <span class="flex-1 truncate pr-6 text-center font-medium text-black dark:text-fg {hideWhenCollapsed}">{parent.label}</span>
              </button>
            </li>
            {#each parent.children as child (child.href)}
              {#if child.sectionStart}
                <li class="my-2 border-t border-neutral-200 dark:border-white/[0.06]" aria-hidden="true"></li>
              {/if}
              <li>
                <a
                  title={child.label}
                  href={child.href}
                  use:link
                  aria-current={leafActive(child) ? 'page' : undefined}
                  class={navClass(child)}
                >
                  {#if child.brandIcon}
                    <span class="menu-item-icon">
                      <span
                        class="block size-full bg-current"
                        style="mask: url('/svgs/{child.brandIcon}.svg') center / contain no-repeat; -webkit-mask: url('/svgs/{child.brandIcon}.svg') center / contain no-repeat;"
                      ></span>
                    </span>
                  {:else}
                    <Reicon name={child.icon ?? ''} class="menu-item-icon" />
                  {/if}
                  <span class="menu-item-label {hideWhenCollapsed}">{child.label}</span>
                </a>
              </li>
              {#if leafActive(child) && page.navChildren}
                <li id="main-nav-active-children" class="empty:hidden ml-5 border-l border-neutral-200 pl-2 dark:border-white/[0.08] {hideWhenCollapsed}">
                  {@render page.navChildren()}
                </li>
              {/if}
            {/each}
          </ul>
        </li>
      {/if}
    {/each}
    {#if !showContext && !submenu}
      <li class="contents">
        <ul role="list" class="flex flex-col gap-y-0.5 {navSwap && 'nav-swap-' + navSwap}">
          {#each sections as [sectionLabel, items], si (sectionLabel)}
            {#if si > 0}
              <li
                class="nav-section mt-3 {collapsed
                  ? 'lg:mx-2.5 lg:my-2 lg:h-0 lg:overflow-hidden lg:border-t lg:border-neutral-200 lg:p-0 lg:text-transparent dark:lg:border-white/10'
                  : ''}"
                aria-hidden="true"
              >
                {sectionLabel}
              </li>
            {/if}
            {#each items as item (item.label)}
              <li>
                {#if isParent(item)}
                  <button
                    type="button"
                    title={item.label}
                    onclick={() => {
                      submenu = item.label;
                      navSwap = 'forward';
                    }}
                    class="{navClass(item)} w-full"
                  >
                    <Reicon name={item.icon} class="menu-item-icon" />
                    <span class="menu-item-label flex-1 text-left {hideWhenCollapsed}">{item.label}</span>
                    <Reicon name="chevron-down" class="nav-chevron size-3 -rotate-90 text-coollabs dark:text-fg-faint {hideWhenCollapsed}" />
                  </button>
                {:else}
                  <a title={item.label} href={item.href} use:link class={navClass(item)}>
                    <Reicon name={item.icon ?? ''} class="menu-item-icon" />
                    <span class="menu-item-label flex-1 {hideWhenCollapsed}">{item.label}</span>
                  </a>
                {/if}
              </li>
            {/each}
          {/each}
        </ul>
      </li>
    {/if}
    <li class="flex-1" aria-hidden="true"></li>
  </ul>

  <div class="sidebar-footer shrink-0 -mx-2 hidden flex-col gap-1 bg-neutral-50 px-2 py-2 dark:bg-transparent lg:-mx-3 lg:flex lg:px-3">
    <div class="flex items-center gap-1 {collapsed ? 'flex-col justify-center' : 'justify-between'}">
      <UserMenu sidebar />
      <button
        type="button"
        onclick={() => layout.toggleSidebar()}
        title="Toggle sidebar"
        aria-label="Toggle sidebar"
        class="menu-item sidebar-toggle w-8 shrink-0 justify-center px-0"
      >
        <svg class="menu-item-icon" viewBox="0 0 24 24" fill="none">
          <rect x="3" y="5" width="18" height="14" rx="4" stroke="currentColor" stroke-width="1.6" />
          <path d="M16 9v6" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" />
        </svg>
      </button>
    </div>
  </div>
  {#if collapsed && tooltip.show}
    <div
      style="left: {tooltip.x}px; top: {tooltip.y}px;"
      class="fixed z-[10000] -translate-y-1/2 px-2 py-1 text-xs font-medium rounded-lg bg-neutral-900 dark:bg-raised text-white whitespace-nowrap pointer-events-none shadow-dropdown border border-neutral-700 dark:border-white/10"
    >
      {tooltip.text}
    </div>
  {/if}
</nav>
