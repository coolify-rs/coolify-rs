<script lang="ts">
  // Port of upstream components/process-dialog.blade.php (the dialog; the
  // trigger is rendered by the caller), teleported to <body> as upstream does.
  // The running/finished/failed chips show only around an activity log.
  import type { Snippet } from 'svelte';
  import Reicon from './Reicon.svelte';
  import { portal } from '../lib/portal';

  let {
    title,
    onclose,
    children,
    closeWithX = false,
    mobileFullscreen = false,
    size = 'lg',
  }: {
    title: string;
    onclose: () => void;
    children: Snippet;
    closeWithX?: boolean;
    mobileFullscreen?: boolean;
    size?: 'md' | 'lg' | 'xl';
  } = $props();

  const width = $derived(
    size === 'md'
      ? 'min-w-0 w-full max-w-2xl sm:min-w-[28rem]'
      : size === 'xl'
        ? 'min-w-0 w-full max-w-5xl sm:min-w-[36rem] lg:min-w-[48rem]'
        : 'min-w-0 w-full max-w-4xl sm:min-w-[32rem] lg:min-w-[42rem]',
  );
</script>

<svelte:window onkeydown={(e) => !closeWithX && e.key === 'Escape' && onclose()} />

<div class="relative z-99" use:portal>
  <div
    role="presentation"
    class="fixed inset-0 bg-black/50 backdrop-blur-[2px] dark:bg-black/60"
    onclick={() => !closeWithX && onclose()}
  ></div>
  <div class="fixed inset-0 overflow-y-auto">
    <div
      role="presentation"
      class="flex min-h-full items-center justify-center {mobileFullscreen ? 'p-0 sm:p-6' : 'p-4 sm:p-6'}"
      onclick={(e) => !closeWithX && e.target === e.currentTarget && onclose()}
    >
      <div
        role="dialog"
        aria-modal="true"
        aria-labelledby="process-dialog-title"
        class="application-settings-section application-settings-form process-dialog relative flex flex-col overflow-hidden {mobileFullscreen
          ? 'process-dialog-mobile-fullscreen'
          : ''} {width} min-h-[min(70dvh,28rem)] h-[min(85dvh,52rem)] max-h-[calc(100dvh-2rem)]"
        style="box-shadow: 0 0 0 1px var(--coollabs-hairline), var(--shadow-modal)"
      >
        <header class="flex-nowrap!">
          <div class="flex min-w-0 flex-1 items-center gap-2">
            <h3 id="process-dialog-title" class="min-w-0 truncate">{title}</h3>
            <!-- Shown by the vendored CSS from the activity monitor's data-activity-status. -->
            <div
              class="process-dialog-status process-dialog-status-running text-accent! inline-flex items-center justify-center gap-2 text-[13px] text-neutral-500 dark:text-fg-dim"
              role="status"
              aria-live="polite"
            >
              <svg class="loading-indicator shrink-0 animate-spin size-3" viewBox="0 0 24 24" fill="none" aria-hidden="true">
                <circle class="opacity-20" cx="12" cy="12" r="9" stroke="currentColor" stroke-width="2" />
                <path class="opacity-80" d="M21 12a9 9 0 0 0-9-9" stroke="currentColor" stroke-width="2" stroke-linecap="round" />
              </svg>
            </div>
            <span class="process-dialog-status process-dialog-status-finished items-center gap-1.5 text-xs text-neutral-500 dark:text-fg-dim" role="status">
              <span class="size-2 rounded-full bg-success"></span>Finished
            </span>
            <span class="process-dialog-status process-dialog-status-failed items-center gap-1.5 text-xs text-neutral-500 dark:text-fg-dim" role="status">
              <span class="size-2 rounded-full bg-error"></span>Failed
            </span>
          </div>
          <button
            type="button"
            onclick={onclose}
            class="flex size-7 shrink-0 cursor-pointer items-center justify-center rounded-md text-neutral-500 outline-0 transition-colors hover:bg-neutral-100 hover:text-black focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-accent dark:text-fg-faint dark:hover:bg-white/[0.06] dark:hover:text-fg"
            aria-label="Close"
          >
            <Reicon name="x" class="size-4" />
          </button>
        </header>
        <div
          class="application-settings-section-body process-dialog-body flex min-h-0 flex-1 flex-col overflow-hidden p-3 sm:p-4"
          style="-webkit-overflow-scrolling: touch;"
        >
          {@render children()}
        </div>
      </div>
    </div>
  </div>
</div>
