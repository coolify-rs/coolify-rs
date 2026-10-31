<script lang="ts">
  // Port of upstream auth/login.blade.php (components/auth/shell.blade.php markup).
  import { onMount } from 'svelte';
  import AuthAlert from '../components/AuthAlert.svelte';
  import AuthShell from '../components/AuthShell.svelte';
  import ErrorPage from '../components/ErrorPage.svelte';
  import PasswordInput from '../components/PasswordInput.svelte';
  import PasskeyVerify from '../components/PasskeyVerify.svelte';
  import { ApiError, sessionApi, type SessionInfo } from '../lib/api';
  import { authUi, errorList, passkeyLogin, type GuestState } from '../lib/authApi';
  import { session, takeLoginError } from '../lib/session.svelte';
  import { link, router } from '../lib/router.svelte';
  import { focusOnMount } from '../lib/focus';

  let email = $state('');
  let password = $state('');
  let busy = $state(false);
  let guest = $state<GuestState | null>(null);
  let errors = $state<string[]>([]);
  let flashError = $state<string | null>(takeLoginError());
  let status = $state<string | null>(null);
  let throttled = $state(false);
  let tooltip = $state(false);
  let tipTimer: ReturnType<typeof setTimeout> | undefined;
  let tip: HTMLElement | undefined = $state();
  let tipHost: HTMLElement | undefined = $state();
  // Upstream renders `<x-forms.button isHighlighted>` as a bare attribute the CSS matches.
  const highlighted = { ishighlighted: '' };

  onMount(async () => {
    try {
      const g = await authUi.state();
      // `Fortify::loginView`: a new instance goes to the registration page.
      if (g.redirect_to_register) {
        router.go('/register', true);
        return;
      }
      guest = g;
      status = g.status;
      flashError = g.error ?? flashError;
      if (g.errors.length) errors = g.errors;
    } catch (e) {
      errors = errorList(e);
    }
  });

  async function submit(e: SubmitEvent) {
    e.preventDefault();
    if (busy) return;
    busy = true;
    errors = [];
    status = null;
    flashError = null;
    try {
      const res = await sessionApi.login({ email, password });
      if ('two_factor' in res) {
        router.go('/two-factor-challenge');
        return;
      }
      session.set(res as SessionInfo);
      router.go('/', true);
    } catch (err) {
      if (err instanceof ApiError && err.status === 429) throttled = true;
      errors = errorList(err);
      // The redirect back shows the form empty.
      email = '';
      password = '';
      document.getElementById('email')?.focus();
    } finally {
      busy = false;
    }
  }

  function passkeyDone(response: unknown) {
    session.set(response as SessionInfo);
    router.go('/', true);
  }

  // The Alpine tooltip of the disabled "Forgot password?" link.
  function showTip() {
    tooltip = true;
    queueMicrotask(() =>
      requestAnimationFrame(() => {
        if (!tip || !tipHost) return;
        const r = tipHost.getBoundingClientRect();
        const t = tip.getBoundingClientRect();
        let top = r.top - t.height - 6;
        let left = r.left;
        if (top < 4) top = r.bottom + 6;
        if (left + t.width > innerWidth - 8) left = innerWidth - 8 - t.width;
        if (left < 4) left = 4;
        tip.style.top = `${top}px`;
        tip.style.left = `${left}px`;
      }),
    );
  }
</script>

{#if throttled}
  <ErrorPage code={429} />
{:else}
<AuthShell title="coolify-rs" description="Sign in to manage your applications and infrastructure.">
  <div class="flex flex-col gap-4">
    {#if status}<AuthAlert type="success">{status}</AuthAlert>{/if}
    {#if flashError}<AuthAlert type="error">{flashError}</AuthAlert>{/if}
    {#if errors.length}<AuthAlert type="error" messages={errors} />{/if}
    <form onsubmit={submit} class="flex flex-col gap-4">
      <div class="w-full">
        <div class="mb-1.5 flex h-4 w-full items-center gap-1.5">
          <label for="email" class="mb-0! flex items-center gap-1 text-sm font-medium leading-4"
            >Email
            <span class="text-helper">*</span>
          </label>
        </div>
        <input
          autocomplete={guest?.passkeys_available ? 'email webauthn' : 'email'}
          class="input"
          required
          type="email"
          id="email"
          name="email"
          bind:value={email}
          use:focusOnMount
        />
      </div>
      <div class="w-full">
        <div class="mb-1.5 flex h-4 w-full items-center gap-1.5">
          <label for="password" class="mb-0! flex items-center gap-1 text-sm font-medium leading-4"
            >Password
            <span class="text-helper">*</span>
          </label>
        </div>
        <PasswordInput id="password" name="password" autocomplete="current-password" required bind:value={password} />
      </div>
      <div class="flex justify-end">
        {#if guest?.transactional_emails}
          <a href="/forgot-password" use:link class="auth-text-link">Forgot password?</a>
        {:else}
          <!-- svelte-ignore a11y_no_static_element_interactions -->
          <span
            bind:this={tipHost}
            class="relative inline-flex"
            onmouseenter={() => (tipTimer = setTimeout(showTip, 300))}
            onmouseleave={() => {
              clearTimeout(tipTimer);
              tooltip = false;
            }}
            onfocusin={showTip}
            onfocusout={() => (tooltip = false)}
          >
            <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
            <span tabindex="0" role="link" aria-disabled="true" class="auth-text-link cursor-not-allowed opacity-50" aria-describedby="forgot-password-disabled-tooltip">
              Forgot password?
            </span>
            <div bind:this={tip} id="forgot-password-disabled-tooltip" style:display={tooltip ? null : 'none'} class="auth-tooltip max-w-xs whitespace-normal">
              Password reset is unavailable because transactional email (SMTP or Resend) is not configured on this instance.
            </div>
          </span>
        {/if}
      </div>
      <button {...highlighted} class="button w-full justify-center" type="submit" disabled={busy}>Login</button>
    </form>

    {#if guest?.passkeys_available}
      <PasskeyVerify routes={passkeyLogin} autofill done={passkeyDone} />
    {/if}

    {#if guest?.enabled_oauth_providers.length}
      <div class="auth-divider"><span>Or continue with</span></div>
      <div class="flex flex-col gap-2">
        {#each guest.enabled_oauth_providers as p (p.provider)}
          <button class="button w-full justify-center" type="button" onclick={() => (document.location.href = `/auth/${p.provider}/redirect`)}>
            {#if p.provider !== 'oidc'}
              <img class="size-5 shrink-0 dark:invert" src="/svgs/{p.provider}.svg" alt="" aria-hidden="true" />
            {/if}
            {p.label}
          </button>
        {/each}
      </div>
    {/if}
  </div>

  {#snippet footer()}
    {#if guest?.is_registration_enabled}
      <span>New to coolify-rs?</span>
      <a href="/register" use:link class="auth-text-link">Register</a>
    {:else if guest?.can_register_with_oauth}
      <span>Only OAuth registration is enabled.</span>
    {:else if guest}
      <span>Registration is disabled. Please contact the administrator.</span>
    {/if}
  {/snippet}
</AuthShell>
{/if}
