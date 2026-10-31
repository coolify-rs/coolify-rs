<script lang="ts">
  // Port of upstream components/error-page.blade.php with errors/{400,403,404,429}.blade.php.
  import { link } from '../lib/router.svelte';

  let { code, description }: { code: 400 | 403 | 404 | 429; description?: string } = $props();

  // `<body class="error-page-body">` of upstream's error views.
  $effect(() => {
    document.body.classList.add('error-page-body');
    return () => document.body.classList.remove('error-page-body');
  });

  const titles = { 400: 'Bad request', 403: 'You shall not pass!', 404: 'How did you get here?', 429: 'Woah, slow down there!' };
  const descriptions = {
    400: 'The request could not be understood by the server due to malformed syntax.',
    403: "You don't have permission to access this page.",
    404: "Sorry, we couldn't find the page you're looking for.",
    429: "You're making too many requests. Please wait a few seconds before trying again.",
  };
</script>

<!-- The page has a root element between it and `<body>`, so the height is set here. -->
<main class="error-shell" style:height="100dvh">
  <div class="error-shell-content">
    <p class="error-code">{code}</p>
    <h1 class="error-title">{titles[code]}</h1>
    <p class="error-description">{description || descriptions[code]}</p>
    <div class="error-actions">
      <a href="/" onclick={(e) => { e.preventDefault(); history.back(); }}><button type="button" class="button">Go back</button></a>
      <a href="/" use:link><button type="button" class="button">Dashboard</button></a>
      <a target="_blank" rel="noopener noreferrer" href="https://coolify.io/docs/contact">
        <button type="button" class="button">
          Contact support
          <svg class="inline-flex size-3 text-current" fill="none" stroke="currentColor" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M10 6H6a2 2 0 00-2 2v10a2 2 0 002 2h10a2 2 0 002-2v-4M14 4h6m0 0v6m0-6L10 14" /></svg>
        </button>
      </a>
    </div>
  </div>
</main>
