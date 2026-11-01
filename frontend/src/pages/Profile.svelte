<script lang="ts">
  // Port of upstream livewire/profile/index.blade.php (App\Livewire\Profile\Index), the
  // two-factor forms that post to Fortify, and x-unsaved-bar.
  import { untrack } from 'svelte';
  import Reicon from '../components/Reicon.svelte';
  import Callout from '../components/Callout.svelte';
  import ConfirmModal from '../components/ConfirmModal.svelte';
  import CopyInput from '../components/CopyInput.svelte';
  import DangerZone from '../components/DangerZone.svelte';
  import Empty from '../components/Empty.svelte';
  import FormInput from '../components/FormInput.svelte';
  import ProfilePasskeys from '../components/ProfilePasskeys.svelte';
  import StatusBadge from '../components/StatusBadge.svelte';
  import UnsavedBar from '../components/UnsavedBar.svelte';
  import { ApiError } from '../lib/api';
  import { needsPasswordConfirmation, profileUi, type ProfileData } from '../lib/profileApi';
  import { compressAvatar, dataUrl, uploadImage } from '../lib/teamsApi';
  import { router } from '../lib/router.svelte';
  import { session } from '../lib/session.svelte';
  import { notify } from '../lib/toast.svelte';

  let data = $state<ProfileData | null>(null);
  let name = $state('');
  let savedName = $state('');
  let saving = $state(false);
  let newEmail = $state('');
  let code = $state('');
  let currentPassword = $state('');
  let newPassword = $state('');
  let newPasswordConfirmation = $state('');
  let emailModalOpen = $state(false);
  let otp = $state('');
  let showCode = $state(false);
  let processing = $state(false);
  let uploadError = $state<string | null>(null);
  let preview = $state<string | null>(null);
  let deleting = $state(false);
  let showForm = $state(false);
  let fieldErrors = $state<ApiError | null>(null);
  let avatarInput: HTMLInputElement | undefined = $state();
  const highlighted = { ishighlighted: '' };

  // `$refs.newEmailInput?.focus()` and `$el.querySelector('input[name=code]')?.focus()`.
  $effect(() => {
    if (emailModalOpen && !data?.show_verification) queueMicrotask(() => document.getElementById('new_email')?.focus());
  });
  $effect(() => {
    if (data?.status === 'two-factor-authentication-enabled') queueMicrotask(() => document.getElementById('code')?.focus());
  });

  async function load() {
    const d = await profileUi.get();
    data = d;
    if (d.passkeys.show_form) showForm = true;
    name = d.name;
    savedName = d.name;
    newEmail = d.new_email ?? '';
    if (d.show_verification) emailModalOpen = true;
  }
  untrack(load);

  const toastError = (e: unknown) => notify.failure(e instanceof ApiError ? e.message : String(e));

  async function run(action: () => Promise<{ message: string }>) {
    fieldErrors = null;
    try {
      const r = await action();
      notify.success(r.message);
      await load();
      return true;
    } catch (e) {
      if (e instanceof ApiError) fieldErrors = e;
      toastError(e);
      return false;
    }
  }

  async function submit() {
    saving = true;
    const submitted = name;
    if (await run(() => profileUi.submit(submitted))) await session.refresh();
    // The server snapshot holds what was sent: `wire:dirty` is over, valid or not.
    savedName = submitted;
    saving = false;
  }

  async function prepareAvatar(event: Event) {
    const input = event.target as HTMLInputElement;
    const file = input.files?.[0];
    if (!file) return;
    processing = true;
    uploadError = null;
    let compressed: Blob;
    try {
      compressed = await compressAvatar(file);
    } catch {
      processing = false;
      uploadError = 'The image could not be processed in this browser.';
      return;
    }
    try {
      const r = await uploadImage('/ui/profile/avatar', compressed, session.info?.csrf_token ?? '');
      notify.success(r.message);
      preview = await dataUrl(compressed);
      await Promise.all([load(), session.refresh()]);
    } catch (e) {
      if (e instanceof ApiError) notify.failure(e.message);
      else uploadError = 'The image could not be uploaded.';
    } finally {
      processing = false;
    }
  }

  async function removeAvatar() {
    preview = null;
    if (await run(() => profileUi.removeAvatar())) await session.refresh();
  }

  async function requestEmail(e: SubmitEvent) {
    e.preventDefault();
    await run(() => profileUi.requestEmail(newEmail));
  }

  async function verifyEmail(e: SubmitEvent) {
    e.preventDefault();
    if (await run(() => profileUi.verifyEmail(code))) {
      if (data && !data.show_verification) {
        emailModalOpen = false;
        code = '';
        await session.refresh();
      }
    }
  }

  async function closeEmailModal() {
    if (data?.show_verification) {
      code = '';
      await run(() => profileUi.cancelEmail());
    }
    emailModalOpen = false;
  }

  async function changePassword(e: SubmitEvent) {
    e.preventDefault();
    fieldErrors = null;
    try {
      const r = await profileUi.password(currentPassword, newPassword, newPasswordConfirmation);
      notify.success(r.message);
      // Every session ended: the reload lands on the login page.
      setTimeout(() => location.reload(), 300);
    } catch (err) {
      if (err instanceof ApiError) fieldErrors = err;
      toastError(err);
    }
  }

  /** A Fortify form: `password.confirm` first, then a reload of the page with the flash status. */
  async function fortify(action: () => Promise<unknown>) {
    showCode = false;
    try {
      await action();
      otp = '';
      await load();
      await session.refresh();
    } catch (e) {
      if (needsPasswordConfirmation(e)) router.go('/user/confirm-password?intended=/profile');
      else toastError(e);
    }
  }

  async function removeAccount(password: string): Promise<string | void> {
    try {
      await profileUi.deleteAccount(password);
      await session.refresh();
      router.go('/login', true);
    } catch (e) {
      if (e instanceof ApiError && e.field('password')) return e.field('password');
      throw e;
    }
  }

  const dirty = $derived(name !== savedName);
  const status = $derived(data?.status);
  const twoFactorSetup = $derived(status === 'two-factor-authentication-enabled');
</script>

<svelte:head><title>Profile | coolify-rs</title></svelte:head>

{#if data}
  <div>
    <div class="mt-8 flex w-full max-w-none flex-col gap-6 lg:mt-3">
      <form
        onsubmit={(e) => {
          e.preventDefault();
          submit();
        }}
      >
        <UnsavedBar {dirty} {saving} onsave={submit} onreset={() => (name = savedName)} />
        <section class="application-settings-section">
          <div class="application-settings-section-header">
            <div>
              <h2>Profile details</h2>
              <p>Your avatar, display name, and verified sign-in address.</p>
            </div>
          </div>
          <div class="application-settings-section-body grid gap-4 sm:grid-cols-2">
            <div class="sm:col-span-2">
              <div class="mb-2 text-sm font-medium leading-4">Avatar</div>
              <div class="flex items-center gap-4">
                <div class="flex size-16 shrink-0 items-center justify-center overflow-hidden rounded-xl bg-neutral-200 text-2xl font-semibold text-neutral-700 dark:bg-white/[0.1] dark:text-fg">
                  {#if preview}
                    <img src={preview} alt="Profile picture preview" class="h-full w-full object-cover" />
                  {:else if data.avatar_url}
                    <img src={data.avatar_url} alt={data.name} class="h-full w-full object-cover" />
                  {:else}
                    <span>{(data.name || data.email).slice(0, 1).toUpperCase()}</span>
                  {/if}
                </div>
                <div class="flex min-w-0 flex-1 flex-col gap-2">
                  <div class="flex flex-wrap gap-2">
                    <input bind:this={avatarInput} type="file" onchange={prepareAvatar} accept="image/jpeg,image/png,image/webp" class="hidden" />
                    <button type="button" class="button" disabled={processing} onclick={() => avatarInput?.click()}>
                      <span>{processing ? 'Uploading…' : 'Browse…'}</span>
                    </button>
                    {#if data.avatar_url}
                      <button type="button" class="button button-error" disabled={processing} onclick={removeAvatar}>Remove</button>
                    {/if}
                  </div>
                  <p class="text-xs text-neutral-500 dark:text-fg-dim">JPG, PNG, or WebP. Shown in the user menu and team member lists.</p>
                  {#if uploadError}<p class="text-xs text-red-500">{uploadError}</p>{/if}
                </div>
              </div>
            </div>
            <FormInput id="name" label="Name" required bind:value={name} error={fieldErrors?.field('name')} />
            <div class="flex items-end gap-2">
              <FormInput id="email" label="Email" readonly value={data.email} />
              <button type="button" class="button" disabled={emailModalOpen || data.uses_sso} onclick={() => (emailModalOpen = true)}>Change</button>
            </div>
          </div>
        </section>
      </form>

      {#if data.uses_sso}
        <Callout type="info" title="Email managed by SSO">
          Signed in with SSO {#if data.sso_provider_label}({data.sso_provider_label}){/if}. Email is managed by your SSO provider.
        </Callout>
      {/if}

      {#if !data.uses_sso && emailModalOpen}
        <div class="fixed inset-0 z-99 flex h-screen w-screen items-center justify-center p-4">
          <div class="absolute inset-0 h-full w-full bg-black/55 backdrop-blur-[3px]"></div>
          <div
            class="application-settings-form application-settings-section relative w-full max-w-xl overflow-hidden"
            style="box-shadow: 0 0 0 1px var(--coollabs-hairline), var(--shadow-modal)"
          >
            <header>
              <div>
                <h3>{data.show_verification ? 'Verify new email' : 'Change email'}</h3>
                <p class="mt-1 text-xs text-neutral-500 dark:text-fg-dim">
                  {#if data.show_verification}
                    Code sent to {data.new_email}.
                  {:else}
                    A six-digit verification code will be sent to the new address.
                  {/if}
                </p>
              </div>
              <button type="button" onclick={closeEmailModal} class="icon-button shrink-0" aria-label="Close">
                <Reicon name="x" class="size-4" />
              </button>
            </header>
            {#if data.show_verification}
              <form onsubmit={verifyEmail} class="application-settings-section-body space-y-4">
                <FormInput id="email_verification_code" label="Verification code" required inputmode="numeric" maxlength={6} bind:value={code} error={fieldErrors?.field('email_verification_code')} />
                <p class="text-xs text-neutral-500 dark:text-fg-dim">The code expires after {data.code_expiry_minutes} minutes.</p>
                <div class="flex justify-end gap-2">
                  <button type="button" class="button" onclick={() => run(() => profileUi.resend())}>Resend code</button>
                  <button type="submit" class="button" {...highlighted}>Verify email</button>
                </div>
              </form>
            {:else}
              <form onsubmit={requestEmail} class="application-settings-section-body space-y-4">
                <FormInput id="new_email" label="New email address" required type="email" autofocus bind:value={newEmail} error={fieldErrors?.field('new_email')} />
                <div class="flex justify-end">
                  <button type="submit" class="button" {...highlighted}>Send code</button>
                </div>
              </form>
            {/if}
          </div>
        </div>
      {/if}

      <form onsubmit={changePassword}>
        <section class="application-settings-section">
          <div class="application-settings-section-header">
            <div>
              <h2>Password</h2>
              <p>Changing your password signs out every active session.</p>
            </div>
            <button type="submit" class="button">Change password</button>
          </div>
          <div class="application-settings-section-body grid gap-4 sm:grid-cols-2">
            <FormInput id="current_password" label="Current password" required type="password" copyable={false} bind:value={currentPassword} error={fieldErrors?.field('current_password')} />
            <FormInput id="new_password" label="New password" required type="password" copyable={false} bind:value={newPassword} error={fieldErrors?.field('new_password')} />
            <FormInput id="new_password_confirmation" label="Confirm new password" required type="password" copyable={false} bind:value={newPasswordConfirmation} />
          </div>
        </section>
      </form>

      <section class="application-settings-section">
        <div class="application-settings-section-header">
          <div>
            <h2>Two-factor authentication</h2>
            <p>Add a time-based one-time password to protect your account.</p>
          </div>
          {#if data.two_factor_confirmed}
            <StatusBadge label="Enabled" type="success" />
          {:else if !twoFactorSetup}
            <button type="button" class="button" onclick={() => fortify(() => profileUi.enable2fa())}>Configure 2FA</button>
          {/if}
        </div>
        <div class="application-settings-section-body">
          {#if twoFactorSetup}
            <div class="grid gap-6 lg:grid-cols-[320px_minmax(0,1fr)]">
              <div class="flex aspect-square items-center justify-center rounded-[10px] border border-neutral-200 bg-white p-5 dark:border-white/[0.07]">
                <!-- eslint-disable-next-line svelte/no-at-html-tags -->
                {@html data.two_factor.qr_svg ?? ''}
              </div>
              <div class="space-y-4">
                <div>
                  <h3 class="text-sm font-semibold text-black dark:text-fg">Finish setup</h3>
                  <p class="mt-1 text-sm text-neutral-500 dark:text-fg-dim">Scan the QR code, then enter the current code from your authenticator.</p>
                </div>
                <form
                  class="flex items-end gap-2"
                  onsubmit={(e) => {
                    e.preventDefault();
                    fortify(() => profileUi.confirm2fa(otp));
                  }}
                >
                  <FormInput id="code" name="code" label="One-time code" required inputmode="numeric" pattern="[0-9]*" autofocus bind:value={otp} />
                  <button type="submit" class="button">Validate 2FA</button>
                </form>
                <div>
                  {#if showCode}
                    <div class="space-y-2 pb-3">
                      <CopyInput text={data.two_factor.secret ?? ''} />
                      <CopyInput text={data.two_factor.url ?? ''} />
                    </div>
                  {/if}
                  <button type="button" class="button" onclick={() => (showCode = !showCode)}>
                    <span>{showCode ? 'Hide manual setup' : 'Show manual setup'}</span>
                  </button>
                </div>
              </div>
            </div>
          {:else if data.two_factor_confirmed}
            <div class="flex flex-col gap-4">
              <div class="flex flex-wrap items-center justify-end gap-2">
                <button type="button" class="button" onclick={() => fortify(() => profileUi.recoveryCodes())}>Regenerate recovery codes</button>
                <button type="button" class="button button-error" onclick={() => fortify(() => profileUi.disable2fa())}>Disable 2FA</button>
              </div>
              {#if data.two_factor.recovery_codes}
                <div class="grid gap-2 rounded-lg border border-neutral-200 bg-neutral-50 p-4 font-mono text-xs text-neutral-700 sm:grid-cols-2 dark:border-white/[0.07] dark:bg-white/[0.05] dark:text-fg-dim">
                  {#each data.two_factor.recovery_codes as c (c)}
                    <div>{c}</div>
                  {/each}
                </div>
              {/if}
            </div>
          {:else}
            <Empty size="sm" title="Two-factor authentication is off" description="Configure an authenticator app to add another sign-in check." iconName="keys" />
          {/if}
        </div>
      </section>

      <ProfilePasskeys {data} bind:showForm onchange={load} />

      <section class="application-settings-section">
        <div class="application-settings-section-header">
          <div>
            <h2>Danger zone</h2>
            <p>Destructive actions for your account cannot be undone.</p>
          </div>
        </div>
        <div class="application-settings-section-body">
          <DangerZone title="Delete account">
            {#if data.account_blockers.length === 0}
              <p>Permanently delete your account from coolify-rs. This action cannot be undone.</p>
              <ul class="space-y-1 text-xs">
                <li>• Teams where you are the only member are deleted.</li>
                <li>• You are removed from all other teams.</li>
                <li>• Your API tokens and sessions are revoked.</li>
              </ul>
            {:else}
              <p>Before you can delete your account:</p>
              <ul class="space-y-1">
                {#each data.account_blockers as blocker (blocker)}
                  <li>• {blocker}</li>
                {/each}
              </ul>
            {/if}
            {#snippet action()}
              {#if data && data.account_blockers.length === 0}
                <div class="relative h-auto max-w-full inline-flex w-auto">
                  <button type="button" class="button button-error flex gap-2" onclick={() => (deleting = true)}>Delete account</button>
                </div>
              {:else}
                <span class="relative inline-flex" title="Resolve the requirements shown before deleting your account.">
                  <button type="button" class="button button-error" disabled>Delete account</button>
                </span>
              {/if}
            {/snippet}
          </DangerZone>
        </div>
      </section>

      {#if data.has_errors}
        <Callout type="danger" title="Profile update failed">Something went wrong. Please review the fields and try again.</Callout>
      {/if}
    </div>
  </div>

  {#if deleting}
    <ConfirmModal
      title="Confirm Account Deletion?"
      actions={data.account_actions}
      confirmWithText={data.confirm.text}
      confirmWithPassword={data.confirm.password}
      confirmationText={data.email}
      confirmationLabel="Enter your email address to confirm permanent deletion"
      shortConfirmationLabel="Email"
      step3ButtonText="Permanently Delete"
      onconfirm={removeAccount}
      onclose={() => (deleting = false)}
    />
  {/if}
{/if}
