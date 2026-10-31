<script lang="ts">
  // The route `/email/verify/{id}/{hash}` (Controller::email_verify): a bad link is a 403 page.
  import { untrack } from 'svelte';
  import ErrorPage from '../components/ErrorPage.svelte';
  import { authUi } from '../lib/authApi';
  import { router } from '../lib/router.svelte';

  let denied = $state(false);

  untrack(async () => {
    try {
      await authUi.emailVerify(router.path, router.search);
      router.go('/', true);
    } catch {
      denied = true;
    }
  });
</script>

{#if denied}
  <ErrorPage code={403} />
{:else}
  <main class="h-full bg-neutral-50 dark:bg-base"></main>
{/if}
