<script lang="ts">
  // Port of upstream livewire/two-factor-required.blade.php (App\Livewire\TwoFactorRequired). The
  // buttons go through the Fortify routes of /profile, which answer with a status for this view.
  import { onMount, tick } from 'svelte';
  import AuthAlert from '../components/AuthAlert.svelte';
  import AuthShell from '../components/AuthShell.svelte';
  import CopyInput from '../components/CopyInput.svelte';
  import FormInput from '../components/FormInput.svelte';
  import { sessionApi } from '../lib/api';
  import { authUi, type TwoFactorRequiredData } from '../lib/authApi';
  import { needsPasswordConfirmation, profileUi } from '../lib/profileApi';
  import { link, router } from '../lib/router.svelte';
  import { session } from '../lib/session.svelte';
  import { notify } from '../lib/toast.svelte';

  let data = $state<TwoFactorRequiredData | null>(null);
  let code = $state('');
  let showCode = $state(false);
  let busy = $state(false);
  const highlighted = { ishighlighted: '' };
  // The view passes `{{ $teamName }}` in an attribute: the name arrives escaped once.
  const escaped = (t: string) => t.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;').replace(/'/g, '&#039;');

  async function load() {
    const d = await authUi.twoFactorRequired();
    if (d.redirect) {
      await session.refresh();
      router.go(d.redirect, true);
      return;
    }
    data = d;
    code = '';
    await tick();
    document.getElementById('code')?.focus();
  }
  onMount(load);

  async function run(action: () => Promise<unknown>) {
    if (busy) return;
    busy = true;
    try {
      await action();
      await session.refresh();
      await load();
    } catch (err) {
      if (needsPasswordConfirmation(err)) router.go('/user/confirm-password?intended=/two-factor-required');
      else notify.failure(err instanceof Error ? err.message : String(err));
    } finally {
      busy = false;
    }
  }

  async function logout(e: SubmitEvent) {
    e.preventDefault();
    await sessionApi.logout();
    session.set(null);
    router.go('/login', true);
  }
</script>

{#if data}
  <AuthShell title="Two-factor authentication required" description="{data.team_name ? escaped(data.team_name) : 'Your team'} requires two-factor authentication. Configure it to continue." footer={data.confirmed ? undefined : footer}>
    <div class="flex flex-col gap-4">
      {#if data.confirmed}
        <p class="text-sm text-neutral-500 dark:text-fg-dim">Two-factor authentication is on. Save these recovery codes in a safe place.</p>
        <div class="grid gap-2 rounded-lg border border-neutral-200 bg-neutral-50 p-4 font-mono text-xs text-neutral-700 sm:grid-cols-2 dark:border-white/[0.07] dark:bg-white/[0.05] dark:text-fg-dim">
          {#each data.recovery_codes ?? [] as c (c)}<div>{c}</div>{/each}
        </div>
        <a href="/" use:link class="button w-full justify-center">Continue</a>
      {:else if data.secret}
        <div class="flex items-center justify-center rounded-[10px] border border-neutral-200 bg-white p-5 dark:border-white/[0.07]">
          <!-- eslint-disable-next-line svelte/no-at-html-tags -->
          {@html data.qr_svg}
        </div>
        <p class="text-sm text-neutral-500 dark:text-fg-dim">Scan the QR code, then enter the current code from your authenticator.</p>
        <form
          class="flex flex-col gap-4"
          onsubmit={(e) => {
            e.preventDefault();
            run(() => profileUi.confirm2fa(code));
          }}
        >
          <FormInput id="code" type="text" inputmode="numeric" pattern="[0-9]*" label="One-time code" required bind:value={code} />
          {#if data.code_error}<AuthAlert type="error">{data.code_error}</AuthAlert>{/if}
          <button class="button w-full justify-center" type="submit" disabled={busy} {...highlighted}>Validate 2FA</button>
        </form>
        <div>
          {#if showCode}
            <div class="space-y-2 pb-3">
              <CopyInput text={data.secret} />
              <CopyInput text={data.url ?? ''} />
            </div>
          {/if}
          <button class="button w-full justify-center" type="button" onclick={() => (showCode = !showCode)}>
            <span>{showCode ? 'Hide manual setup' : 'Show manual setup'}</span>
          </button>
        </div>
      {:else}
        <form
          onsubmit={(e) => {
            e.preventDefault();
            run(() => profileUi.enable2fa());
          }}
        >
          <button class="button w-full justify-center" type="submit" disabled={busy} {...highlighted}>Configure 2FA</button>
        </form>
      {/if}
    </div>
  </AuthShell>
{:else}
  <main class="h-full bg-neutral-50 dark:bg-base"></main>
{/if}

{#snippet footer()}
  <span>Not now?</span>
  <form onsubmit={logout}>
    <button type="submit" class="auth-text-link">Log out</button>
  </form>
{/snippet}
