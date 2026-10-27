<script lang="ts">
  // Port of upstream components/security/settings-layout.blade.php.
  import type { Snippet } from 'svelte';

  let { actions, children }: { actions?: Snippet; children: Snippet } = $props();
</script>

<section class="w-full max-w-none">
  <header class="settings-mobile-header xl:hidden">
    <h1 class="settings-mobile-title">Keys & Tokens</h1>
    <p class="settings-mobile-description">Manage SSH keys, cloud credentials, and API access tokens.</p>
  </header>
  <div class="min-w-0">
    {#if actions}
      <div class="mb-3 flex min-h-8 flex-wrap items-center justify-end gap-2">
        {@render actions()}
      </div>
    {/if}
    {@render children()}
  </div>
</section>
