<script lang="ts">
  // Port of upstream components/settings/layout.blade.php. The pages are in the Settings submenu of
  // the main sidebar.
  import type { Snippet } from 'svelte';
  let { children }: { children: Snippet } = $props();
</script>

<section class="application-settings-workspace w-full max-w-none">
  <header class="settings-mobile-header xl:hidden">
    <h1 class="settings-mobile-title">Instance Settings</h1>
    <p class="settings-mobile-description">Configure global settings for this coolify-rs instance.</p>
  </header>
  <div class="min-w-0">
    {@render children()}
  </div>
</section>
