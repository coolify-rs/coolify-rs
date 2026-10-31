<script lang="ts">
  // Port of upstream auth/register.blade.php (Fortify::registerView, CreateNewUser).
  import { onMount } from 'svelte';
  import AuthAlert from '../components/AuthAlert.svelte';
  import AuthShell from '../components/AuthShell.svelte';
  import FormInput from '../components/FormInput.svelte';
  import Reicon from '../components/Reicon.svelte';
  import { authUi, errorList, type GuestState } from '../lib/authApi';
  import { link, router } from '../lib/router.svelte';
  import { session } from '../lib/session.svelte';

  let guest = $state<GuestState | null>(null);
  let name = $state('');
  let email = $state('');
  let password = $state('');
  let password_confirmation = $state('');
  let errors = $state<string[]>([]);
  let busy = $state(false);
  const highlighted = { ishighlighted: '' };

  onMount(async () => {
    const g = await authUi.state();
    if (!g.is_registration_enabled) {
      router.go('/login', true);
      return;
    }
    guest = g;
    requestAnimationFrame(() => document.getElementById('name')?.focus());
  });

  async function submit(e: SubmitEvent) {
    e.preventDefault();
    if (busy) return;
    busy = true;
    errors = [];
    try {
      const res = await authUi.register({ name, email, password, password_confirmation });
      session.set(res);
      router.go(res.redirect, true);
    } catch (err) {
      errors = errorList(err);
      password = '';
      password_confirmation = '';
      if (guest) guest = await authUi.state();
    } finally {
      busy = false;
    }
  }
</script>

{#if guest}
  <AuthShell title="coolify-rs" description={guest.is_first_user ? 'Create the root account for this instance.' : 'Create your account to get started.'} footer={guest.is_first_user ? undefined : footer}>
    <div class="flex flex-col gap-4">
      {#if guest.is_root_setup_window_closed}
        <AuthAlert type="warning">
          <p class="font-medium">Setup window closed</p>
          <p class="mt-0.5 text-black/70 dark:text-white/70">
            You can create the root account only in the first {guest.root_setup_minutes} minutes after coolify-rs starts. To open the window again, run this command on the server, then reload this page:
          </p>
          <code class="mt-2 block font-mono text-xs">docker restart coolify</code>
        </AuthAlert>
      {:else if guest.is_first_user}
        <AuthAlert type="warning">
          <p class="font-medium">Full instance access</p>
          <p class="mt-0.5 text-black/70 dark:text-white/70">
            This first account becomes the root user. Create it within {guest.root_setup_minutes} minutes after coolify-rs starts.
          </p>
        </AuthAlert>
      {/if}

      {#if errors.length}<AuthAlert type="error" messages={errors} />{/if}

      {#if !guest.is_root_setup_window_closed}
        <form onsubmit={submit} class="flex flex-col gap-4">
          <FormInput id="name" required type="text" autocomplete="name" label="Name" bind:value={name} />
          <FormInput id="email" required type="email" autocomplete="email" label="Email" bind:value={email} />
          <FormInput id="password" required type="password" copyable={false} autocomplete="new-password" label="Password" bind:value={password} />
          <FormInput id="password_confirmation" required type="password" copyable={false} autocomplete="new-password" label="Password again" bind:value={password_confirmation} />

          <div class="auth-guidance">
            <Reicon name="info-circle" class="mt-0.5 size-4 shrink-0" />
            <p>Use at least 8 characters with uppercase, lowercase, number, and symbol.</p>
          </div>

          <button class="button w-full justify-center" type="submit" disabled={busy} {...highlighted}>Create account</button>
        </form>
      {/if}
    </div>
  </AuthShell>
{:else}
  <main class="h-full bg-neutral-50 dark:bg-base"></main>
{/if}

{#snippet footer()}
  <span>Already have an account?</span>
  <a href="/login" use:link class="auth-text-link underline">Already registered?</a>
{/snippet}
