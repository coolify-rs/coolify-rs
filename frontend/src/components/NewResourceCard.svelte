<script lang="ts">
  // One resource card of upstream livewire/project/new/select.blade.php
  // (the application and database `<article>` templates).
  import Reicon from './Reicon.svelte';

  let {
    name,
    description,
    logo,
    logoDark,
    source,
    experimental = false,
    docs,
    website,
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
  aria-label="Deploy {name}"
  onclick={onselect}
  onkeydown={key}
  class="group flex min-h-48 cursor-pointer flex-col rounded-xl border border-neutral-200 bg-white p-4 transition-colors hover:border-neutral-300 focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-accent dark:border-white/[0.08] dark:bg-white/[0.05] dark:hover:border-white/[0.14]"
>
  <div class="flex min-w-0 {source ? 'items-start' : 'items-center'} gap-3">
    <div
      class="flex size-11 shrink-0 items-center justify-center overflow-hidden rounded-xl border border-neutral-200 bg-neutral-50 dark:border-white/[0.08] dark:bg-white/[0.04]"
    >
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
          <span
            class="inline-flex h-6 max-w-full items-center gap-1.5 whitespace-nowrap rounded-full border border-neutral-200 bg-neutral-100 px-2 text-xs font-medium leading-none text-neutral-700 dark:border-white/[0.12] dark:bg-white/[0.07] dark:text-white shrink-0"
          >
            <span class="relative flex size-1.5 shrink-0">
              <span class="relative inline-flex size-1.5 rounded-full bg-warning"></span>
            </span>
            <span class="truncate">Experimental</span>
          </span>
        {/if}
      </div>
    {/if}
  </div>

  <p class="mt-3 line-clamp-2 text-[12px] leading-5 text-neutral-600 dark:text-fg-dim">{description}</p>

  <div class="mt-auto flex items-center gap-1.5 border-t border-neutral-200 pt-3 dark:border-white/[0.07]">
    <a class="button" href={docs} target="_blank" rel="noopener noreferrer" onclick={(e) => e.stopPropagation()}>Docs</a>
    {#if website}
      <a href={website} target="_blank" rel="noopener noreferrer" class="button" onclick={(e) => e.stopPropagation()}>Website</a>
    {/if}
    <span class="button button-highlighted ml-auto">
      Deploy
      <Reicon name="arrow-right" class="size-3.5" />
    </span>
  </div>
</article>
