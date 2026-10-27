<script lang="ts">
  // Port of upstream auth/login.blade.php and auth/two-factor-challenge.blade.php
  // (components/auth/shell.blade.php markup).
  import Reicon from '../components/Reicon.svelte';
  import PasswordInput from '../components/PasswordInput.svelte';
  import { ApiError, sessionApi, type SessionInfo } from '../lib/api';
  import { session } from '../lib/session.svelte';
  import { router } from '../lib/router.svelte';
  import { focusOnMount } from '../lib/focus';

  let email = $state('');
  let password = $state('');
  let code = $state('');
  let needsCode = $state(false);
  let busy = $state(false);
  let errors = $state<string[]>([]);
  let codeForm: HTMLFormElement | undefined = $state();
  // Upstream renders `<x-forms.button isHighlighted>` as a bare attribute the CSS matches.
  const highlighted = { ishighlighted: '' };

  async function submit(e: SubmitEvent) {
    e.preventDefault();
    if (busy) return;
    busy = true;
    errors = [];
    try {
      const res = await sessionApi.login({ email, password, code: needsCode ? code : undefined });
      if ('two_factor' in res) {
        needsCode = true;
        return;
      }
      session.set(res as SessionInfo);
      router.go('/', true);
    } catch (err) {
      if (err instanceof ApiError) {
        const all = Object.values(err.errors).flat();
        errors = all.length ? all : [err.message];
      } else errors = [String(err)];
      if (needsCode) code = '';
    } finally {
      busy = false;
    }
  }

  function onCodeInput(e: Event) {
    const input = e.target as HTMLInputElement;
    code = input.value.replace(/\D/g, '').slice(0, 6);
    if (code.length === 6) queueMicrotask(() => codeForm?.requestSubmit());
  }

  function backToLogin(e: MouseEvent) {
    e.preventDefault();
    needsCode = false;
    code = '';
    errors = [];
  }
</script>

{#snippet alert(messages: string[])}
  <div class="flex items-start gap-3 rounded-lg border px-3 py-2.5 text-sm border-error/35 bg-error/10 text-error">
    <Reicon name="alert-circle" class="mt-0.5 size-4 shrink-0" />
    <div class="min-w-0 flex-1 leading-5">
      <div class="flex flex-col gap-1">
        {#each messages as m, i (i)}<p>{m}</p>{/each}
      </div>
    </div>
  </div>
{/snippet}

<main class="h-full bg-neutral-50 dark:bg-base">
  <section class="auth-shell application-settings-form">
    <div class="auth-shell-content">
      <div class="auth-card">
        <div class="auth-card-heading">
          <h1>coolify-rs</h1>
          <p>{needsCode ? 'Verify your identity to finish signing in.' : 'Sign in to manage your applications and infrastructure.'}</p>
        </div>
        <div class="auth-card-body">
          {#if !needsCode}
            <div class="flex flex-col gap-4">
              {#if errors.length}{@render alert(errors)}{/if}
              <form onsubmit={submit} class="flex flex-col gap-4">
                <div class="w-full">
                  <div class="mb-1.5 flex h-4 w-full items-center gap-1.5">
                    <label for="email" class="mb-0! flex items-center gap-1 text-sm font-medium leading-4"
                      >Email
                      <span class="text-helper">*</span>
                    </label>
                  </div>
                  <input
                    autocomplete="email"
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
                  <span class="relative inline-flex">
                    <span
                      tabindex="0"
                      role="link"
                      aria-disabled="true"
                      class="auth-text-link cursor-not-allowed opacity-50"
                      title="Password reset is not available in coolify-rs yet."
                    >
                      Forgot password?
                    </span>
                  </span>
                </div>
                <button {...highlighted} class="button w-full justify-center" type="submit" disabled={busy}>Login</button>
              </form>
            </div>
          {:else}
            <div class="flex flex-col gap-4">
              {#if errors.length}{@render alert(errors)}{/if}
              <div class="auth-guidance">
                <Reicon name="info-circle" class="mt-0.5 size-4 shrink-0" />
                <p>Enter the 6-digit code from your authenticator app.</p>
              </div>
              <form bind:this={codeForm} onsubmit={submit} class="flex flex-col gap-4">
                <div class="flex flex-col gap-3">
                  <input
                    type="text"
                    name="code"
                    inputmode="numeric"
                    pattern="[0-9]*"
                    maxlength="6"
                    autocomplete="one-time-code"
                    aria-label="Two-factor authentication code"
                    value={code}
                    oninput={onCodeInput}
                    use:focusOnMount
                    class="mx-auto h-14 w-64 rounded-md border border-neutral-300 bg-white px-4 text-center text-xl font-semibold tracking-[0.5em] text-neutral-900 transition-colors focus:border-warning focus:outline-none focus:ring-1 focus:ring-warning dark:border-white/10 dark:bg-coolgray-100 dark:text-white"
                  />
                </div>
                <button class="button w-full justify-center" type="submit" disabled={busy} {...highlighted}>Verify and continue</button>
              </form>
            </div>
          {/if}
        </div>
        <footer class="auth-card-footer">
          {#if needsCode}
            <span>Not your account?</span>
            <a href="/login" class="auth-text-link" onclick={backToLogin}>Back to login</a>
          {:else}
            <span>Accounts are managed in Coolify.</span>
          {/if}
        </footer>
      </div>
    </div>
  </section>
</main>
