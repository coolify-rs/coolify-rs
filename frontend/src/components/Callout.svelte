<script lang="ts">
  // Port of upstream components/callout.blade.php.
  import type { Snippet } from 'svelte';
  import Reicon from './Reicon.svelte';

  type Kind = 'warning' | 'danger' | 'info' | 'success';
  let {
    type = 'warning',
    title = 'Warning',
    class: cls = '',
    children,
    ondismiss,
  }: { type?: Kind; title?: string; class?: string; children: Snippet; /** `dismissible` with `onDismiss`. */ ondismiss?: () => void } = $props();

  const styles: Record<Kind, { icon: string; shell: string; iconClass: string; titleClass: string; textClass: string }> = {
    warning: {
      icon: 'alert-triangle',
      // Numbered warning shades: the custom theme maps the plain warning color to its accent.
      shell: 'border-warning-700/25 bg-warning-700/[0.07] dark:border-warning-400/20 dark:bg-warning-400/[0.06]',
      iconClass: 'text-warning-700 dark:text-warning-400',
      titleClass: 'text-warning-900 dark:text-warning-400',
      textClass: 'text-warning-900/75 dark:text-warning-400/70',
    },
    danger: {
      icon: 'alert-circle',
      shell: 'border-red-300/60 bg-red-50 dark:border-red-500/20 dark:bg-red-500/[0.07]',
      iconClass: 'text-red-600 dark:text-red-400',
      titleClass: 'text-red-800 dark:text-red-300',
      textClass: 'text-red-700/80 dark:text-red-300/75',
    },
    info: {
      icon: 'info-circle',
      shell: 'border-coollabs/20 bg-coollabs/[0.055] dark:border-brand-light/15 dark:bg-brand/[0.06]',
      iconClass: 'text-coollabs dark:text-brand-light',
      titleClass: 'text-coollabs dark:text-brand-light',
      textClass: 'text-neutral-600 dark:text-fg-dim',
    },
    success: {
      icon: 'check-circle',
      shell: 'border-emerald-300/60 bg-emerald-50 dark:border-emerald-500/20 dark:bg-emerald-500/[0.07]',
      iconClass: 'text-emerald-600 dark:text-emerald-400',
      titleClass: 'text-emerald-800 dark:text-emerald-300',
      textClass: 'text-emerald-700/80 dark:text-emerald-300/75',
    },
  };
  const style = $derived(styles[type]);
</script>

<div class="relative rounded-lg border px-3 py-2.5 {style.shell} {cls}">
  <div class="flex items-start gap-2.5">
    <Reicon name={style.icon} class="mt-0.5 size-4 shrink-0 {style.iconClass}" />
    <div class="min-w-0 flex-1 {ondismiss ? 'pr-7' : ''}">
      <div class="text-[12px] font-semibold {style.titleClass}">{title}</div>
      <div class="mt-0.5 text-[12px] leading-5 {style.textClass}">{@render children()}</div>
    </div>
    {#if ondismiss}
      <button
        type="button"
        onclick={(event) => {
          event.stopPropagation();
          ondismiss();
        }}
        class="absolute top-1.5 right-1.5 flex size-7 items-center justify-center rounded-md transition-colors hover:bg-black/[0.05] dark:hover:bg-white/[0.06]"
        aria-label="Dismiss"
      >
        <Reicon name="x" class="size-3.5 {style.iconClass}" />
      </button>
    {/if}
  </div>
</div>
