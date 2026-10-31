<script lang="ts">
  // Port of upstream components/shared-variables/layout.blade.php.
  import type { Snippet } from 'svelte';

  let { children }: { children: Snippet } = $props();
</script>

<section class="application-settings-workspace w-full max-w-none">
  <header class="mb-6 xl:hidden">
    <h1 class="text-[24px]! leading-7! font-semibold! tracking-tight!">Shared variables</h1>
    <p class="mt-1 text-[13px] text-neutral-500 dark:text-fg-dim">Reusable environment variables across resources</p>
  </header>
  <div class="min-w-0">
    {@render children()}
  </div>
</section>
