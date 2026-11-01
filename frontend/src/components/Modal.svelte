<script lang="ts">
  // Port of upstream components/modal-input.blade.php (the dialog part; the
  // trigger is rendered by the caller).
  import type { Snippet } from 'svelte';
  import Reicon from './Reicon.svelte';

  let {
    title,
    subtitle,
    onclose,
    children,
    footer,
    isLarge = false,
    fixedHeight = false,
    narrow = false,
    widthClass = '',
    focusFirstField = true,
  }: {
    title: string;
    subtitle?: string;
    onclose: () => void;
    children: Snippet;
    footer?: Snippet;
    isLarge?: boolean;
    /** `fixedHeight` of modal-input.blade.php. */
    fixedHeight?: boolean;
    /** The mount dialogs of the storage page. */
    narrow?: boolean;
    /** A dialog with its own width in the Blade view. */
    widthClass?: string;
    /** Upstream does not focus a field in modals opened through `wireOpen`. */
    focusFirstField?: boolean;
  } = $props();

  // Focus the first field when the dialog opens.
  function focusFirst(node: HTMLElement) {
    if (focusFirstField) queueMicrotask(() => node.querySelector<HTMLElement>('input, textarea, select')?.focus());
  }
</script>

<svelte:window onkeydown={(e) => e.key === 'Escape' && onclose()} />

<div class="fixed inset-0 z-99 overflow-hidden" use:focusFirst>
  <div class="absolute inset-0 w-full h-full bg-black/50 backdrop-blur-[2px]"></div>
  <div
    role="presentation"
    onclick={(e) => e.target === e.currentTarget && onclose()}
    class="relative flex min-h-full items-start justify-center p-2 sm:items-center sm:p-4"
  >
    <div
      role="dialog"
      aria-modal="true"
      aria-label={title}
      class="application-settings-form application-settings-section relative flex max-h-[calc(100dvh-2rem)] w-full flex-col overflow-hidden {isLarge
        ? 'lg:w-[95vw]! lg:max-w-7xl!'
        : widthClass ? widthClass : narrow ? 'lg:w-[36rem]!' : 'lg:w-auto lg:min-w-2xl lg:max-w-4xl'} {fixedHeight ? 'sm:h-[40rem]' : ''}"
      style="box-shadow: 0 0 0 1px var(--coollabs-hairline), var(--shadow-modal)"
    >
      <header class="shrink-0 flex-wrap! sm:flex-nowrap!">
        <div class="min-w-0 flex-1 py-0.5">
          <h3 class="truncate">{title}</h3>
          {#if subtitle}
            <p class="mt-0.5 text-xs text-neutral-500 dark:text-fg-dim">{subtitle}</p>
          {/if}
        </div>
        <button
          type="button"
          onclick={onclose}
          aria-label="Close"
          class="order-2 sm:order-none flex size-7 shrink-0 cursor-pointer items-center justify-center rounded-md text-neutral-500 outline-0 transition-colors hover:bg-neutral-100 hover:text-black focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-accent dark:text-fg-faint dark:hover:bg-white/[0.06] dark:hover:text-fg"
        >
          <Reicon name="x" class="size-4" />
        </button>
      </header>
      <div class="application-settings-section-body min-h-0 flex-1 overflow-y-auto" style="-webkit-overflow-scrolling: touch;">
        {@render children()}
      </div>
      {#if footer}
        <footer class="flex shrink-0 flex-wrap items-center justify-end gap-2 border-t border-neutral-200 px-4 py-3 dark:border-white/[0.08]">
          {@render footer()}
        </footer>
      {/if}
    </div>
  </div>
</div>
