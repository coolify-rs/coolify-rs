<script lang="ts">
  // Port of upstream auth/verify-email.blade.php and livewire/verify-email.blade.php
  // (App\Livewire\VerifyEmail).
  import AuthShell from '../components/AuthShell.svelte';
  import Reicon from '../components/Reicon.svelte';
  import { ApiError } from '../lib/api';
  import { authUi } from '../lib/authApi';
  import { notify } from '../lib/toast.svelte';

  const highlighted = { ishighlighted: '' };

  async function again() {
    try {
      notify.success((await authUi.resendVerification()).message);
    } catch (err) {
      notify.failure(err instanceof ApiError ? err.message : String(err));
    }
  }
</script>

<AuthShell title="coolify-rs" description="Verify your email address to activate your account.">
  <div class="flex flex-col gap-4">
    <div class="auth-guidance">
      <Reicon name="mail" class="mt-0.5 size-4 shrink-0" />
      <p>We sent a verification link to your email address. Open it to continue to coolify-rs.</p>
    </div>
    <div>
      <button class="button w-full justify-center" type="button" onclick={again} {...highlighted}>Resend verification email</button>
    </div>
  </div>
  {#snippet footer()}
    <span class="text-center">
      <span class="block sm:inline">Didn’t receive the email?</span>
      <span class="block sm:ml-1 sm:inline">Check your spam folder or resend it.</span>
    </span>
  {/snippet}
</AuthShell>
