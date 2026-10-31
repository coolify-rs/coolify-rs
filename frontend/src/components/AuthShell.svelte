<script lang="ts">
  // Port of upstream components/auth/shell.blade.php inside layouts/simple.
  import type { Snippet } from 'svelte';

  let { title, description, children, footer }: { title: string; description?: string; children: Snippet; footer?: Snippet } = $props();
</script>

<main class="h-full bg-neutral-50 dark:bg-base">
  <section class="auth-shell application-settings-form">
    <div class="auth-shell-content">
      <div class="auth-card">
        <div class="auth-card-heading">
          <h1>{title}</h1>
          {#if description}<p>{description}</p>{/if}
        </div>
        <div class="auth-card-body">
          {@render children()}
        </div>
        {#if footer}
          <footer class="auth-card-footer">{@render footer()}</footer>
        {/if}
      </div>
    </div>
  </section>
</main>
