<script lang="ts">
  // Port of upstream errors/403.blade.php with components/error-page.blade.php: a page of its own, above the shell.
  import { link } from '../lib/router.svelte';
  import { portal } from '../lib/portal';
</script>

<div class="error-page-body fixed inset-0 z-[10000] overflow-auto text-black dark:text-inherit" use:portal>
  <main class="error-shell">
    <div class="error-shell-content">
      <p class="error-code">403</p>
      <h1 class="error-title">You shall not pass!</h1>
      <p class="error-description">You don't have permission to access this page.</p>
      <div class="error-actions">
        <a href="/" onclick={(e) => (e.preventDefault(), history.back())}><button type="button" class="button">Go back</button></a>
        <a href="/" use:link><button type="button" class="button">Dashboard</button></a>
        <a target="_blank" rel="noopener noreferrer" href="https://coolify.io/docs/contact">
          <button type="button" class="button">
            Contact support
            <svg xmlns="http://www.w3.org/2000/svg" class="inline-flex size-3 text-current" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
              <path d="M10 6H6a2 2 0 00-2 2v10a2 2 0 002 2h10a2 2 0 002-2v-4M14 4h6m0 0v6m0-6L10 14" />
            </svg>
          </button>
        </a>
      </div>
    </div>
  </main>
</div>
