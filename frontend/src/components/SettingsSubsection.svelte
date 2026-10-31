<script lang="ts">
  // Port of upstream components/application/settings-subsection.blade.php.
  import type { Snippet } from 'svelte';
  import Helper from './Helper.svelte';

  let { title, helper, first = false, children }: { title: string; helper?: string; first?: boolean; children: Snippet } = $props();
</script>

<section class={first ? '' : 'mt-5 border-t border-neutral-200 pt-5 dark:border-white/[0.07]'}>
  <div class="mb-3 flex flex-wrap items-center justify-between gap-3">
    <h3 class="flex items-center gap-1.5 text-sm! font-semibold! text-black dark:text-fg">
      {title}
      {#if helper}
        <Helper {helper} label="More information about {title}" />
      {/if}
    </h3>
  </div>
  {@render children()}
</section>
