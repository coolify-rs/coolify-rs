<script lang="ts">
  // Port of upstream components/top-breadcrumb.blade.php: team switcher,
  // then either the section name or the project/environment switchers.
  import { tick } from 'svelte';
  import TeamSwitcher from './TeamSwitcher.svelte';
  import BreadcrumbSwitcher from './BreadcrumbSwitcher.svelte';
  import { page, dashboardContext } from '../lib/page.svelte';
  import { router, link } from '../lib/router.svelte';

  const context = $derived(dashboardContext(router.path));
  // Pages inside a project (its environments and their resources) show the switchers they set.
  const inProject = (name: string) =>
    ['project', 'environment', 'resource', 'database', 'application', 'service', 'destination', 'source-'].some((prefix) => name.startsWith(prefix));
  const switchers = $derived(inProject(router.current.name) ? page.switchers : []);

  // Hide the section name when the page heading right below already says it.
  let titleRepeats = $state(false);
  $effect(() => {
    const expected = context ?? '';
    void router.path;
    tick().then(() =>
      requestAnimationFrame(() => {
        const heading = document.querySelector<HTMLElement>('.app-main h1');
        const title = heading && heading.offsetHeight > 0 && heading.getBoundingClientRect().top < 140 ? heading.textContent?.trim() : '';
        titleRepeats = expected !== '' && title === expected;
      }),
    );
  });

  let openIndex = $state(-1);
  let root: HTMLDivElement | undefined = $state();
</script>

<svelte:window
  onclick={(e) => openIndex >= 0 && root && !root.contains(e.target as Node) && (openIndex = -1)}
  onkeydown={(e) => e.key === 'Escape' && (openIndex = -1)}
/>

<div class="flex min-w-0 items-center gap-0.5 text-[13px]" data-page-context={context ?? ''} bind:this={root}>
  <div class="shrink-0">
    <TeamSwitcher />
  </div>

  {#if (switchers.length === 0 || router.current.name === 'destination' || page.sourceConnected !== null) && context && !titleRepeats}
    <span class="shrink-0 px-0.5 text-neutral-300 dark:text-fg-faint">/</span>
    <span class="flex h-8 min-w-0 items-center truncate px-2 font-semibold text-black opacity-70 dark:text-fg">{context}</span>
  {/if}

  {#if page.resourceSwitcher}
    <BreadcrumbSwitcher switcher={page.resourceSwitcher} />
  {/if}

  {#each switchers as s, i (s.heading)}
    <span class="shrink-0 text-neutral-300 dark:text-fg-faint px-0.5">/</span>
    <div class="relative min-w-0 shrink">
      <button
        type="button"
        onclick={() => (openIndex = openIndex === i ? -1 : i)}
        title={s.title}
        class="flex items-center gap-1.5 min-w-0 max-w-full h-8 px-2 rounded-md opacity-70 transition-[background-color,opacity] hover:opacity-100 hover:bg-neutral-100 dark:hover:bg-white/[0.05]"
      >
        <span class="min-w-0 truncate font-semibold text-black dark:text-fg">{s.label}</span>
        <svg class="size-4 shrink-0 text-neutral-400 dark:text-fg-faint" viewBox="0 0 24 24" fill="none">
          <path d="M8 9l4-4 4 4M8 15l4 4 4-4" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" />
        </svg>
      </button>
      {#if openIndex === i}
        <div class="listbox-panel scrollbar left-0! z-[90]! max-h-80! {i === 0 ? 'min-w-56' : 'min-w-52'} max-w-72">
          <div class="px-2 py-1 text-[10px] font-semibold uppercase tracking-wide text-neutral-400 dark:text-fg-faint">{s.heading}</div>
          {#each s.items as item (item.href)}
            <a
              href={item.href}
              use:link
              onclick={() => (openIndex = -1)}
              class="listbox-option {item.active ? 'bg-neutral-100 font-medium text-black dark:bg-white/[0.07] dark:text-fg' : ''}"
            >
              <span class="min-w-0 flex-1 truncate">{item.label}</span>
            </a>
          {/each}
        </div>
      {/if}
    </div>
    {#if s.meta}
      <span class="inline-flex h-[22px] shrink-0 items-center gap-1.5 rounded-full border border-neutral-200 bg-neutral-100 px-2.5 text-xs font-medium text-black dark:border-white/[0.12] dark:bg-white/[0.08] dark:text-fg">
        <span class="size-1.5 rounded-full {s.meta.dot}"></span>
        {s.meta.label}
      </span>
    {/if}
  {/each}

  {#if page.sourceConnected !== null && switchers.length > 0}
    <span
      class="inline-flex h-[22px] shrink-0 items-center gap-1.5 rounded-full border border-neutral-200 bg-neutral-100 px-2.5 text-xs font-medium text-black dark:border-white/[0.12] dark:bg-white/[0.07] dark:text-white"
    >
      <span class="size-1.5 rounded-full {page.sourceConnected ? 'bg-[#3fb950]' : 'bg-warning'}"></span>
      {page.sourceConnected ? 'Connected' : 'Setup incomplete'}
    </span>
  {/if}
</div>
