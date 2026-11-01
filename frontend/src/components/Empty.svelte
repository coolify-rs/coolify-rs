<script lang="ts">
  // Port of upstream components/empty.blade.php.
  import type { Snippet } from 'svelte';
  import Reicon from './Reicon.svelte';

  let {
    title,
    description,
    size = 'base',
    iconName,
    iconClass,
    contents,
  }: {
    title: string;
    description?: string;
    size?: 'sm' | 'base' | 'lg';
    iconName?: string;
    /** The class of the `x-reicon` in `<x-slot:icon>`, when it is not the default size. */
    iconClass?: string;
    contents?: Snippet;
  } = $props();

  const minHeight = $derived({ sm: 'min-h-32', lg: 'min-h-96', base: 'min-h-40' }[size]);
  const iconBox = $derived({ sm: 'mb-3 size-10', lg: 'mb-4 size-12', base: 'mb-4 size-11' }[size]);
  const iconSize = $derived({ sm: 'size-4.5', lg: 'size-6', base: 'size-5' }[size]);
  const descriptionClass = $derived(size === 'sm' ? 'text-[12px]' : 'text-[13px]');
</script>

<div class="empty-state w-full">
  <div class="empty-state-well {minHeight}">
    {#if iconName}
      <div
        class="{iconBox} flex items-center justify-center rounded-xl border border-neutral-200 bg-white text-neutral-400 shadow-sm dark:border-white/[0.08] dark:bg-white/[0.035] dark:text-fg-faint"
      >
        <Reicon name={iconName} class={iconClass ?? iconSize} />
      </div>
    {/if}
    <h2 class="empty-state-title">{title}</h2>
    {#if description}
      <p class="empty-state-description {descriptionClass}">{description}</p>
    {/if}
    {#if contents}
      <div class="mt-4 flex flex-wrap items-center justify-center gap-2">
        {@render contents()}
      </div>
    {/if}
  </div>
</div>
