<script lang="ts">
  // Port of upstream auth/reset-password.blade.php (NewPasswordController, ResetUserPassword).
  import { onMount } from 'svelte';
  import AuthAlert from '../components/AuthAlert.svelte';
  import AuthShell from '../components/AuthShell.svelte';
  import FormInput from '../components/FormInput.svelte';
  import Reicon from '../components/Reicon.svelte';
  import { ApiError } from '../lib/api';
  import { authUi, errorList } from '../lib/authApi';
  import { link, router } from '../lib/router.svelte';

  let { token, email }: { token: string; email: string } = $props();

  let password = $state('');
  let password_confirmation = $state('');
  let errors = $state<string[]>([]);
  let failure = $state<ApiError | null>(null);
  let busy = $state(false);
  const highlighted = { ishighlighted: '' };

  onMount(() => document.getElementById('password')?.focus());

  async function submit(e: SubmitEvent) {
    e.preventDefault();
    if (busy) return;
    busy = true;
    errors = [];
    failure = null;
    try {
      await authUi.resetPassword({ token, email, password, password_confirmation });
      router.go('/login', true);
    } catch (err) {
      errors = errorList(err);
      failure = err instanceof ApiError ? err : null;
      password = '';
      password_confirmation = '';
      document.getElementById('password')?.focus();
    } finally {
      busy = false;
    }
  }
</script>

<AuthShell title="Reset password" description="Choose a strong new password for your coolify-rs account.">
  <div class="flex flex-col gap-4">
    {#if errors.length}<AuthAlert type="error" messages={errors} />{/if}
    <form onsubmit={submit} class="flex flex-col gap-4">
      <input hidden id="token" name="token" value={token} />
      <input hidden value={email} type="email" name="email" />
      <FormInput id="password" required type="password" copyable={false} autocomplete="new-password" label="Password" bind:value={password} error={failure?.field('password')} />
      <FormInput id="password_confirmation" required type="password" copyable={false} autocomplete="new-password" label="Password again" bind:value={password_confirmation} />

      <div class="auth-guidance">
        <Reicon name="info-circle" class="mt-0.5 size-4 shrink-0" />
        <p>Use at least 8 characters with uppercase, lowercase, number, and symbol.</p>
      </div>

      <button class="button w-full justify-center" type="submit" disabled={busy} {...highlighted}>Reset password</button>
    </form>
  </div>
  {#snippet footer()}
    <span>Remember your password?</span>
    <a href="/login" use:link class="auth-text-link">Back to login</a>
  {/snippet}
</AuthShell>
