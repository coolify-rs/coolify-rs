<script lang="ts">
  // Port of upstream auth/forgot-password.blade.php (Controller::forgot_password).
  import { onMount } from 'svelte';
  import AuthAlert from '../components/AuthAlert.svelte';
  import AuthShell from '../components/AuthShell.svelte';
  import ErrorPage from '../components/ErrorPage.svelte';
  import { ApiError } from '../lib/api';
  import FormInput from '../components/FormInput.svelte';
  import { authUi, errorList, type GuestState } from '../lib/authApi';
  import { link } from '../lib/router.svelte';

  let guest = $state<GuestState | null>(null);
  let email = $state('');
  let status = $state<string | null>(null);
  let errors = $state<string[]>([]);
  let throttled = $state(false);
  let busy = $state(false);
  const highlighted = { ishighlighted: '' };

  onMount(async () => {
    guest = await authUi.state();
    status = guest.status;
    errors = guest.errors;
    requestAnimationFrame(() => document.getElementById('email')?.focus());
  });

  async function submit(e: SubmitEvent) {
    e.preventDefault();
    if (busy) return;
    busy = true;
    errors = [];
    status = null;
    try {
      status = (await authUi.forgotPassword(email)).status;
    } catch (err) {
      if (err instanceof ApiError && err.status === 429) throttled = true;
      errors = errorList(err);
    } finally {
      // The redirect back shows the form empty.
      email = '';
      document.getElementById('email')?.focus();
      busy = false;
    }
  }
</script>

{#if throttled}
  <ErrorPage code={429} />
{:else if guest}
  <AuthShell title="coolify-rs" description="Enter your account email and we’ll send you a secure reset link.">
    <div class="flex flex-col gap-4">
      {#if status}<AuthAlert type="success">{status}</AuthAlert>{/if}
      {#if errors.length}<AuthAlert type="error" messages={errors} />{/if}
      {#if guest.transactional_emails}
        <form onsubmit={submit} class="flex flex-col gap-4">
          <FormInput id="email" required type="email" autocomplete="email" label="Email" bind:value={email} />
          <button class="button w-full justify-center" type="submit" disabled={busy} {...highlighted}>Send password reset email</button>
        </form>
      {:else}
        <AuthAlert type="warning">
          <p class="font-medium">Transactional email is not configured</p>
          <p class="mt-0.5 text-black/70 dark:text-white/70">
            Configure email delivery or follow the
            <a class="font-medium underline underline-offset-2" target="_blank" rel="noopener noreferrer" href="https://coolify.io/docs">manual reset guide</a>.
          </p>
        </AuthAlert>
      {/if}
    </div>
    {#snippet footer()}
      <span>Remember your password?</span>
      <a href="/login" use:link class="auth-text-link">Back to login</a>
    {/snippet}
  </AuthShell>
{:else}
  <main class="h-full bg-neutral-50 dark:bg-base"></main>
{/if}
