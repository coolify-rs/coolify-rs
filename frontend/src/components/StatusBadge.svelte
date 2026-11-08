<script lang="ts">
  // Port of upstream components/status-badge.blade.php (span and button variants).
  import type { Snippet } from 'svelte';

  let {
    label = '',
    type = 'neutral',
    class: klass = '',
    as = 'span',
    title,
    onclick,
    pulse = false,
    size = 'md',
    spinning = false,
    children,
  }: {
    label?: string;
    type?: 'neutral' | 'success' | 'warning' | 'error';
    class?: string;
    as?: 'span' | 'button';
    title?: string;
    onclick?: (e: MouseEvent) => void;
    /** Ping the dot for a state that is still in progress. */
    pulse?: boolean;
    /** `sm` fits inline next to 12px table text without growing the row. */
    size?: 'sm' | 'md';
    /** Show the loading spinner instead of the dot, e.g. for a running deployment. */
    spinning?: boolean;
    /** `dynamic`: the caller renders the dot and the label. */
    children?: Snippet;
  } = $props();

  const dot = {
    neutral: 'bg-neutral-400 dark:bg-neutral-500',
    success: 'bg-emerald-500',
    warning: 'bg-warning',
    error: 'bg-red-500',
  };
  const sizes = {
    sm: 'h-4 gap-1 px-1.5 text-[10px]',
    md: 'h-6 gap-1.5 px-2 text-xs',
  };
  const base = $derived(
    `inline-flex max-w-full items-center whitespace-nowrap rounded-full border border-neutral-200 bg-neutral-100 font-medium leading-none text-neutral-700 dark:border-white/[0.12] dark:bg-white/[0.07] dark:text-white ${sizes[size]}`,
  );
</script>

{#snippet content()}
  {#if children}
    {@render children()}
  {:else}
    {#if spinning}
      <svg class="loading-indicator size-3 shrink-0 animate-spin text-neutral-500 dark:text-fg-dim" viewBox="0 0 24 24" fill="none" aria-hidden="true">
        <circle class="opacity-20" cx="12" cy="12" r="9" stroke="currentColor" stroke-width="2" />
        <path class="opacity-80" d="M21 12a9 9 0 0 0-9-9" stroke="currentColor" stroke-width="2" stroke-linecap="round" />
      </svg>
    {:else}
      <span class="relative flex size-1.5 shrink-0">
        {#if pulse}
          <span class="absolute inline-flex size-full rounded-full opacity-75 motion-safe:animate-ping {dot[type]}"></span>
        {/if}
        <span class="relative inline-flex size-1.5 rounded-full {dot[type]}"></span>
      </span>
    {/if}
    <span class="truncate">{label}</span>
  {/if}
{/snippet}

{#if as === 'button'}
  <button type="button" class="{base} transition-colors {klass}" {title} {onclick}>{@render content()}</button>
{:else}
  <span class="{base} {klass}" {title}>{@render content()}</span>
{/if}
