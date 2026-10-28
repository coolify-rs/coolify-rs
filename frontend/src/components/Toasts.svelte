<script lang="ts">
  // Port of upstream components/toast.blade.php (bottom-right position).
  import Reicon from './Reicon.svelte';
  import { sanitizeHtml } from '../lib/sanitize';
  import { toasts, type Toast } from '../lib/toast.svelte';

  const iconTone: Record<Toast['type'], string> = {
    success: 'bg-emerald-100 text-emerald-700 dark:bg-emerald-500/10 dark:text-emerald-400',
    info: 'bg-coollabs/10 text-coollabs dark:bg-brand/15 dark:text-brand-light',
    warning: 'bg-amber-100 text-amber-700 dark:bg-warning/10 dark:text-warning',
    danger: 'bg-red-100 text-red-700 dark:bg-red-500/10 dark:text-red-400',
    default: 'bg-neutral-100 text-neutral-600 dark:bg-white/[0.06] dark:text-fg-dim',
  };
  const icon: Record<Toast['type'], string> = {
    success: 'check-circle',
    info: 'info-circle',
    warning: 'alert-triangle',
    danger: 'alert-circle',
    default: 'info-circle',
  };

  async function copy(t: Toast) {
    try {
      await navigator.clipboard.writeText([t.message, t.description].filter(Boolean).join('\n'));
      t.copied = true;
      setTimeout(() => (t.copied = false), 1500);
    } catch {
      // Clipboard access can be denied; nothing to do.
    }
  }
</script>

<ul class="fixed z-9999 flex w-[calc(100%-2rem)] gap-2.5 sm:max-w-[26rem] right-4 bottom-4 flex-col-reverse" aria-live="polite">
  {#each toasts.items as t (t.id)}
    <li
      onmouseenter={() => toasts.pause(t.id)}
      onmouseleave={() => toasts.schedule(t.id, 4000)}
      class="surface-popover toast-card relative flex w-full items-start rounded-lg group p-3.5 pr-20"
    >
      <div class="flex min-w-0 items-start gap-3">
        <div class="toast-icon flex size-8 shrink-0 items-center justify-center rounded-lg {iconTone[t.type]}">
          <Reicon name={icon[t.type]} class="size-4" />
        </div>
        <div class="min-w-0 flex-1 pt-0.5">
          <p class="text-sm font-semibold leading-5 text-neutral-950 dark:text-fg">{t.message}</p>
          {#if t.description}
            <div class="mt-0.5 w-full whitespace-pre-wrap break-words text-xs leading-5 text-neutral-600 dark:text-fg-dim">{@html sanitizeHtml(t.description)}</div>
          {/if}
        </div>
      </div>
      {#if t.description}
        <button
          type="button"
          onclick={() => copy(t)}
          title={t.copied ? 'Copied' : 'Copy details'}
          class="absolute right-10 top-2.5 flex size-7 items-center justify-center rounded-md text-neutral-400 opacity-0 transition-colors hover:bg-black/5 hover:text-neutral-700 group-hover:opacity-100 dark:text-fg-faint dark:hover:bg-white/[0.06] dark:hover:text-fg {t.copied
            ? 'bg-emerald-100 text-emerald-700 dark:bg-emerald-500/10 dark:text-emerald-400'
            : ''}"
        >
          {#if t.copied}
            <Reicon name="check" class="size-3.5" />
          {:else}
            <svg class="size-3.5" xmlns="http://www.w3.org/2000/svg" fill="none" viewBox="0 0 24 24" stroke-width="1.7" stroke="currentColor">
              <path
                stroke-linecap="round"
                stroke-linejoin="round"
                d="M8.25 7.5V6a2.25 2.25 0 012.25-2.25h7.5A2.25 2.25 0 0120.25 6v7.5A2.25 2.25 0 0118 15.75h-1.5m-8.25-8.25H6A2.25 2.25 0 003.75 9.75v7.5A2.25 2.25 0 006 19.5h7.5a2.25 2.25 0 002.25-2.25V15m-7.5-7.5h5.25A2.25 2.25 0 0115.75 9.75V15"
              />
            </svg>
          {/if}
        </button>
      {/if}
      <button
        type="button"
        onclick={() => toasts.dismiss(t.id)}
        aria-label="Dismiss"
        class="absolute right-2.5 top-2.5 flex size-7 items-center justify-center rounded-md text-neutral-400 transition-colors hover:bg-black/5 hover:text-neutral-700 dark:text-fg-faint dark:hover:bg-white/[0.06] dark:hover:text-fg"
      >
        <Reicon name="x" class="size-3.5" />
      </button>
    </li>
  {/each}
</ul>
