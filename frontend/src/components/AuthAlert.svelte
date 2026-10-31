<script lang="ts">
  // Port of upstream components/auth/alert.blade.php.
  import type { Snippet } from 'svelte';
  import Reicon from './Reicon.svelte';

  let { type = 'info', messages, children }: { type?: 'success' | 'error' | 'warning' | 'info'; messages?: string[]; children?: Snippet } = $props();

  const styles = $derived(
    {
      success: 'border-success/35 bg-success/10 text-success',
      error: 'border-error/35 bg-error/10 text-error',
      warning: 'border-warning/35 bg-warning/10 text-warning',
      info: 'border-neutral-300 bg-neutral-100 text-neutral-700 dark:border-white/10 dark:bg-white/[0.04] dark:text-fg-dim',
    }[type],
  );
  const icon = $derived({ success: 'check-circle', error: 'alert-circle', warning: 'alert-triangle', info: 'info-circle' }[type]);
</script>

<div class="flex items-start gap-3 rounded-lg border px-3 py-2.5 text-sm {styles}">
  <Reicon name={icon} class="mt-0.5 size-4 shrink-0" />
  <div class="min-w-0 flex-1 leading-5">
    {#if messages}
      <div class="flex flex-col gap-1">
        {#each messages as m, i (i)}<p>{m}</p>{/each}
      </div>
    {:else}
      {@render children?.()}
    {/if}
  </div>
</div>
