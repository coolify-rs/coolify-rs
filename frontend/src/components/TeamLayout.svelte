<script lang="ts">
  // Port of upstream components/team/settings-layout.blade.php.
  import type { Snippet } from 'svelte';

  let { children }: { children: Snippet } = $props();
</script>

<section class="w-full max-w-none">
  <header class="settings-mobile-header xl:hidden">
    <h1 class="settings-mobile-title">Team</h1>
    <p class="settings-mobile-description">Manage your team, members, and access settings.</p>
  </header>
  <div class="min-w-0">
    {@render children()}
  </div>
</section>
