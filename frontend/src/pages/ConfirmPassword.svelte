<script lang="ts">
  // Port of upstream auth/confirm-password.blade.php (Fortify's password.confirm screen). Coming
  // back from it goes to the `intended` page, /profile unless a page asked for another.
  import AuthShell from '../components/AuthShell.svelte';
  import Reicon from '../components/Reicon.svelte';
  import FormInput from '../components/FormInput.svelte';
  import { ApiError } from '../lib/api';
  import PasskeyVerify from '../components/PasskeyVerify.svelte';
  import { authUi, passkeyConfirm } from '../lib/authApi';
  import { profileUi } from '../lib/profileApi';
  import { router } from '../lib/router.svelte';
  import { onMount } from 'svelte';

  let canUsePasskey = $state(false);
  onMount(async () => {
    document.getElementById('password')?.focus();
    canUsePasskey = (await authUi.confirmState().catch(() => ({ can_confirm_with_passkey: false }))).can_confirm_with_passkey;
  });
  const afterConfirm = () => router.go(router.query('intended') ?? '/profile', true);

  let password = $state('');
  let errors = $state<string[]>([]);
  let busy = $state(false);
  const highlighted = { ishighlighted: '' };

  async function submit(e: SubmitEvent) {
    e.preventDefault();
    if (busy) return;
    busy = true;
    errors = [];
    try {
      await profileUi.confirmPassword(password);
      router.go(router.query('intended') ?? '/profile', true);
    } catch (err) {
      errors = err instanceof ApiError ? Object.values(err.errors).flat() : [String(err)];
      if (!errors.length) errors = [err instanceof Error ? err.message : String(err)];
      password = '';
      queueMicrotask(() => document.getElementById('password')?.focus());
    } finally {
      busy = false;
    }
  }
</script>

<AuthShell title="coolify-rs" description="Confirm your password to continue to this secure area.">
  <div class="flex flex-col gap-4">
    {#if errors.length}
      <div class="flex items-start gap-3 rounded-lg border px-3 py-2.5 text-sm border-error/35 bg-error/10 text-error">
        <Reicon name="alert-circle" class="mt-0.5 size-4 shrink-0" />
        <div class="min-w-0 flex-1 leading-5">
          <div class="flex flex-col gap-1">
            {#each errors as m, i (i)}<p>{m}</p>{/each}
          </div>
        </div>
      </div>
    {/if}
    <div class="auth-guidance">
      <Reicon name="info-circle" class="mt-0.5 size-4 shrink-0" />
      <p>This is a secure area. Please confirm your password before continuing.</p>
    </div>
    <form onsubmit={submit} class="flex flex-col gap-4">
      <FormInput id="password" label="Password" type="password" required copyable={false} autocomplete="current-password" bind:value={password} />
      <button class="button w-full justify-center" type="submit" disabled={busy} {...highlighted}>Confirm password</button>
    </form>
    {#if canUsePasskey}
      <PasskeyVerify routes={passkeyConfirm} label="Confirm with a passkey" done={afterConfirm} />
    {/if}
  </div>
</AuthShell>
