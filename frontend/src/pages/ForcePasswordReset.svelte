<script lang="ts">
  // Port of upstream livewire/force-password-reset.blade.php (App\Livewire\ForcePasswordReset).
  import { onMount } from 'svelte';
  import AuthShell from '../components/AuthShell.svelte';
  import FormInput from '../components/FormInput.svelte';
  import Reicon from '../components/Reicon.svelte';
  import { ApiError } from '../lib/api';
  import { authUi } from '../lib/authApi';
  import { router } from '../lib/router.svelte';
  import { session } from '../lib/session.svelte';
  import { notify } from '../lib/toast.svelte';

  let email = $state(session.info?.user.email ?? '');
  let password = $state('');
  let password_confirmation = $state('');
  let busy = $state(false);
  const highlighted = { ishighlighted: '' };

  onMount(() => document.getElementById('password')?.focus());

  async function submit(e: SubmitEvent) {
    e.preventDefault();
    if (busy) return;
    busy = true;
    try {
      const res = await authUi.forceReset({ email, password, password_confirmation });
      // The password change ended the session: the dashboard redirect arrives at the login page.
      if (res.signed_out) {
        session.set(null);
        router.go('/login', true);
      } else router.go('/', true);
    } catch (err) {
      // `handleError`: a toast only, no message under the field.
      notify.failure(err instanceof ApiError ? err.message : String(err));
    } finally {
      busy = false;
    }
  }
</script>

<AuthShell title="Set a new password" description="Your password must be updated before you can continue to the dashboard.">
  <form class="flex flex-col gap-4" onsubmit={submit}>
    <FormInput id="email" type="email" readonly label="Email" value={email} />
    <FormInput id="password" type="password" copyable={false} autocomplete="new-password" label="New password" required bind:value={password} />
    <FormInput id="password_confirmation" type="password" copyable={false} autocomplete="new-password" label="Confirm new password" required bind:value={password_confirmation} />

    <div class="auth-guidance">
      <Reicon name="info-circle" class="mt-0.5 size-4 shrink-0" />
      <p>Use at least 8 characters with uppercase, lowercase, number, and symbol.</p>
    </div>

    <button class="button w-full justify-center" type="submit" disabled={busy} {...highlighted}>Reset password</button>
  </form>
</AuthShell>
