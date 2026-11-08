<script lang="ts">
  // Port of upstream auth/two-factor-challenge.blade.php (TwoFactorAuthenticatedSessionController).
  import { onMount, tick } from 'svelte';
  import AuthAlert from '../components/AuthAlert.svelte';
  import AuthShell from '../components/AuthShell.svelte';
  import ErrorPage from '../components/ErrorPage.svelte';
  import { ApiError } from '../lib/api';
  import FormInput from '../components/FormInput.svelte';
  import Reicon from '../components/Reicon.svelte';
  import { authUi, errorList } from '../lib/authApi';
  import { link, router } from '../lib/router.svelte';
  import { session } from '../lib/session.svelte';

  let ready = $state(false);
  let showRecovery = $state(false);
  let submitting = $state(false);
  let throttled = $state(false);
  let code = $state('');
  let codeFocused = $state(false);
  let recovery = $state('');
  let errors = $state<string[]>([]);
  let status = $state<string | null>(null);
  let form: HTMLFormElement | undefined = $state();
  let codeInput: HTMLInputElement | undefined = $state();
  const highlighted = { ishighlighted: '' };

  onMount(async () => {
    const g = await authUi.state();
    // `hasChallengedUser`.
    if (!g.two_factor_challenge) {
      router.go('/login', true);
      return;
    }
    status = g.status;
    errors = g.errors;
    ready = true;
    await tick();
    codeInput?.focus();
  });

  function onCodeInput(e: Event) {
    const input = e.target as HTMLInputElement;
    code = input.value.replace(/\D/g, '').slice(0, 6);
    input.value = code;
    if (code.length === 6) queueMicrotask(() => form?.requestSubmit());
  }

  async function toggle(toRecovery: boolean) {
    showRecovery = toRecovery;
    await tick();
    if (toRecovery) document.getElementById('recovery_code')?.focus();
    else codeInput?.focus();
  }

  async function submit(e: SubmitEvent) {
    e.preventDefault();
    if (submitting) return;
    submitting = true;
    errors = [];
    status = null;
    try {
      const res = await authUi.twoFactor(showRecovery ? { recovery_code: recovery } : { code });
      session.set(res);
      router.go('/', true);
    } catch (err) {
      // The redirect back shows the page as new: the code form, with the error.
      if (err instanceof ApiError && err.status === 429) throttled = true;
      errors = errorList(err);
      code = '';
      recovery = '';
      showRecovery = false;
      if (codeInput) codeInput.value = '';
      await tick();
      codeInput?.focus();
    } finally {
      submitting = false;
    }
  }
</script>

{#if throttled}
  <ErrorPage code={429} />
{:else if ready}
  <AuthShell title="coolify-rs" description="Verify your identity to finish signing in.">
    <div class="flex flex-col gap-4">
      {#if status}<AuthAlert type="success">{status}</AuthAlert>{/if}
      {#if errors.length}<AuthAlert type="error" messages={errors} />{/if}

      <div class="auth-guidance">
        <Reicon name="info-circle" class="mt-0.5 size-4 shrink-0" />
        {#if showRecovery}
          <p>Enter one of the recovery codes you saved when setting up two-factor authentication.</p>
        {:else}
          <p>Enter the 6-digit code from your authenticator app.</p>
        {/if}
      </div>

      <form bind:this={form} onsubmit={submit} class="flex flex-col gap-4">
        <div class="flex flex-col gap-3" style:display={showRecovery ? 'none' : null}>
          <!-- 607daf80b: the code shows as six digit boxes over one transparent input. -->
          <div class="relative mx-auto flex gap-2">
            {#each [0, 1, 2, 3, 4, 5] as i (i)}
              <div
                aria-hidden="true"
                data-code-digit
                class="flex h-14 w-11 items-center justify-center rounded-md border bg-white text-xl font-semibold text-neutral-900 transition-colors dark:bg-coolgray-100 dark:text-white {codeFocused &&
                (code.length === i || (code.length === 6 && i === 5))
                  ? 'border-coollabs ring-1 ring-coollabs'
                  : 'border-neutral-300 dark:border-white/10'}"
              >
                {code[i] ?? ''}
              </div>
            {/each}
            <input
              bind:this={codeInput}
              type="text"
              name="code"
              inputmode="numeric"
              pattern="[0-9]*"
              maxlength="6"
              autocomplete="one-time-code"
              aria-label="Two-factor authentication code"
              disabled={showRecovery}
              oninput={onCodeInput}
              onfocus={() => (codeFocused = true)}
              onblur={() => (codeFocused = false)}
              class="absolute inset-0 h-full w-full cursor-text rounded-md border-0 bg-transparent text-transparent caret-transparent opacity-0 focus:outline-none focus:ring-0"
            />
          </div>
          <button type="button" class="auth-text-link self-center" onclick={() => toggle(true)}>Use a recovery code</button>
        </div>

        <div class="flex flex-col gap-3" style:display={showRecovery ? null : 'none'}>
          <FormInput id="recovery_code" autocomplete="one-time-code" disabled={!showRecovery} label="Recovery code" bind:value={recovery} />
          <button type="button" class="auth-text-link self-center" onclick={() => toggle(false)}>Use an authenticator code</button>
        </div>

        <button class="button w-full justify-center" type="submit" disabled={submitting} {...highlighted}>Verify and continue</button>
      </form>
    </div>
    {#snippet footer()}
      <span>Not your account?</span>
      <a href="/login" use:link class="auth-text-link">Back to login</a>
    {/snippet}
  </AuthShell>
{:else}
  <main class="h-full bg-neutral-50 dark:bg-base"></main>
{/if}
