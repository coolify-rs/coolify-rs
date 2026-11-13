<script lang="ts" module>
  /** The classes of one view of the picker (`viewClasses` of upstream livewire/project/new/select.blade.php). */
  export interface NewResourceViewClasses {
    container: string;
    item: string;
    logo: string;
    header: string;
    description: string;
    footer: string;
    action: string;
  }

  /** `viewClasses`: a list of rows, or a grid of cards. */
  export function newResourceViewClasses(viewMode: 'list' | 'grid'): NewResourceViewClasses {
    if (viewMode === 'list') {
      return {
        container: 'flex flex-col divide-y divide-neutral-200 overflow-hidden rounded-[calc(0.5rem-1px)] dark:divide-white/[0.07]',
        item: 'items-center gap-4 px-4 py-2.5 hover:bg-neutral-50 dark:hover:bg-white/[0.025]',
        logo: 'size-8 rounded-lg',
        header: 'flex-1 items-center! lg:w-64 lg:flex-none',
        description: 'hidden min-w-0 flex-1 truncate lg:block',
        footer: 'w-44 shrink-0 justify-end',
        action: '',
      };
    }
    return {
      container: 'grid grid-cols-1 gap-3 md:grid-cols-2 xl:grid-cols-3',
      item: 'min-h-48 flex-col rounded-xl border border-neutral-200 bg-white p-4 hover:border-neutral-300 dark:border-white/[0.08] dark:bg-white/[0.05] dark:hover:border-white/[0.14]',
      logo: 'size-11 rounded-xl',
      header: '',
      description: 'mt-3 line-clamp-2',
      footer: 'mt-auto border-t border-neutral-200 pt-3 dark:border-white/[0.07]',
      action: 'ml-auto',
    };
  }
</script>

<script lang="ts">
  // One resource of upstream livewire/project/new/select.blade.php
  // (the application and database `<article>` templates), as a list row or a grid card.
  import Reicon from './Reicon.svelte';
  import BetaBadge from './BetaBadge.svelte';

  let {
    name,
    description,
    logo,
    logoDark,
    source,
    experimental = false,
    docs,
    website,
    view,
    onselect,
  }: {
    name: string;
    description: string;
    logo: string;
    logoDark?: string;
    /** "Git source" / "Docker source" under the name (applications); databases have none. */
    source?: string;
    experimental?: boolean;
    docs?: string;
    /** Databases always show a Website button. */
    website?: string;
    view: NewResourceViewClasses;
    onselect: () => void;
  } = $props();

  function key(e: KeyboardEvent) {
    if (e.target !== e.currentTarget) return;
    if (e.key === 'Enter' || e.key === ' ') {
      e.preventDefault();
      onselect();
    }
  }
</script>

<article
  role="button"
  tabindex="0"
  aria-label="Configure {name}"
  onclick={onselect}
  onkeydown={key}
  class="group flex cursor-pointer transition-colors focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-accent {view.item}"
>
  <div class="flex min-w-0 {source ? 'items-start' : 'items-center'} gap-3 {view.header}">
    <div class="flex shrink-0 items-center justify-center overflow-hidden border border-neutral-200 bg-neutral-50 dark:border-white/[0.08] dark:bg-white/[0.04] {view.logo}">
      {#if logoDark}
        <span class="size-full">
          <img class="size-full object-contain block dark:hidden" src={logo} alt="" />
          <img class="size-full object-contain hidden dark:block" src={logoDark} alt="" />
        </span>
      {:else}
        <img class="size-full object-contain" src={logo} alt="" />
      {/if}
    </div>
    {#if source}
      <div class="min-w-0 flex-1">
        <h3 class="truncate text-[13px] font-semibold text-black dark:text-fg">{name}</h3>
        <p class="mt-0.5 text-[11px] text-neutral-500 dark:text-fg-faint">{source}</p>
      </div>
    {:else}
      <div class="flex min-w-0 flex-1 items-center gap-2">
        <h3 class="truncate text-[13px] font-semibold text-black dark:text-fg">{name}</h3>
        {#if experimental}
          <BetaBadge class="shrink-0" />
        {/if}
      </div>
    {/if}
  </div>

  <p class="text-[12px] leading-5 text-neutral-600 dark:text-fg-dim {view.description}">{description}</p>

  <div class="flex items-center gap-1.5 {view.footer}">
    <a class="icon-button" title="Documentation" aria-label="Documentation" href={docs} target="_blank" rel="noopener noreferrer" onclick={(e) => e.stopPropagation()}>
      <Reicon name="documentation" class="size-4" />
    </a>
    {#if website}
      <a href={website} target="_blank" rel="noopener noreferrer" class="icon-button" title="Website" aria-label="Website" onclick={(e) => e.stopPropagation()}>
        <Reicon name="globe" class="size-4" />
      </a>
    {/if}
    <span class="button {view.action}">
      Configure
      <Reicon name="arrow-right" class="size-3.5" />
    </span>
  </div>
</article>
