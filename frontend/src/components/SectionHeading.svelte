<script lang="ts">
  // Port of upstream components/section-heading.blade.php.
  import type { Snippet } from 'svelte';
  import { link } from '../lib/router.svelte';

  let {
    title,
    subtitle,
    href,
    actionLabel = 'View all',
    titleAction,
    actions,
  }: {
    title: string;
    subtitle?: string;
    href?: string | null;
    actionLabel?: string;
    titleAction?: Snippet;
    actions?: Snippet;
  } = $props();
</script>

{#snippet viewAll()}
  <a
    href={href ?? ''}
    use:link
    class="group inline-flex shrink-0 items-center gap-0.5 text-[11px] text-neutral-500 hover:text-black hover:underline dark:text-fg-faint dark:hover:text-fg"
  >
    {actionLabel}
  </a>
{/snippet}

<div class="mb-3 min-w-0">
  <div class="flex flex-wrap items-center justify-between gap-x-4 gap-y-2">
    <div class="flex min-w-0 items-baseline gap-2">
      <div class="flex min-w-0 items-center gap-1">
        <h2 class="min-w-0 truncate text-[14px]! leading-5! font-semibold! text-black dark:text-fg">{title}</h2>
        {@render titleAction?.()}
      </div>
      {#if href && !actions}{@render viewAll()}{/if}
    </div>
    {#if actions}
      <div class="ml-auto flex shrink-0 items-center gap-3">
        {#if href}{@render viewAll()}{/if}
        {@render actions()}
      </div>
    {/if}
  </div>
  {#if subtitle}
    <p class="mt-0.5 text-[11px] text-neutral-500 dark:text-fg-faint">{subtitle}</p>
  {/if}
</div>
