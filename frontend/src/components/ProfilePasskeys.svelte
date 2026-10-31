<script lang="ts">
  // Port of upstream livewire/profile/passkeys.blade.php (App\Livewire\Profile\Passkeys).
  import Callout from './Callout.svelte';
  import ConfirmModal from './ConfirmModal.svelte';
  import Empty from './Empty.svelte';
  import FormInput from './FormInput.svelte';
  import { ApiError } from '../lib/api';
  import { diffForHumans } from '../lib/format';
  import { passkeyDeviceName, profileUi, registerPasskey, needsPasswordConfirmation, type ProfileData, type PasskeyRow } from '../lib/profileApi';
  import { link, router } from '../lib/router.svelte';
  import { notify } from '../lib/toast.svelte';

  let { data, showForm = $bindable(false), onchange }: { data: ProfileData; showForm?: boolean; onchange: () => Promise<void> } = $props();

  let name = $state(passkeyDeviceName());
  let loading = $state(false);
  let error = $state<string | null>(null);
  let renameError = $state<string | null>(null);
  let editing = $state<number | null>(null);
  let editName = $state('');
  let deleting = $state<PasskeyRow | null>(null);
  const highlighted = { ishighlighted: '' };

  const enabled = $derived(data.passkeys.enabled);
  const available = $derived(data.passkeys.available);

  async function register(e: SubmitEvent) {
    e.preventDefault();
    loading = true;
    error = null;
    try {
      await registerPasskey(name);
      showForm = false;
      await onchange();
      notify.success('Passkey added.');
    } catch (err) {
      if (needsPasswordConfirmation(err)) router.go('/user/confirm-password?intended=/profile/passkeys/confirm');
      else if (!(err instanceof Error && err.name === 'UserCancelledError')) error = err instanceof Error ? err.message : String(err);
    } finally {
      loading = false;
    }
  }

  async function saveRename(e: SubmitEvent, id: number) {
    e.preventDefault();
    renameError = null;
    try {
      await profileUi.renamePasskey(id, editName);
      notify.success('Passkey renamed.');
      editing = null;
      await onchange();
    } catch (err) {
      if (err instanceof ApiError && err.field('name')) renameError = err.field('name') ?? null;
      else notify.failure(err instanceof Error ? err.message : String(err));
    }
  }

  async function remove(password: string): Promise<string | void> {
    if (!deleting) return;
    try {
      await profileUi.deletePasskey(deleting.id, password);
      notify.success('Passkey deleted.');
      await onchange();
    } catch (err) {
      if (err instanceof ApiError && err.field('password')) return err.field('password');
      throw err;
    }
  }
</script>

{#if enabled}
  <div>
    <section class="application-settings-section">
      <div class="application-settings-section-header">
        <div>
          <h2>Passkeys</h2>
          <p>Sign in with your fingerprint, face, or a security key. A passkey sign-in skips two-factor authentication.</p>
        </div>
        {#if available && !showForm}
          <a class="button" href="/profile/passkeys/confirm" use:link>Add passkey</a>
        {/if}
      </div>
      <div class="application-settings-section-body flex flex-col gap-4">
        {#if !available}
          <Callout type="info" title="Passkeys are not available on this address">
            Passkeys need HTTPS on the instance domain, or <code>localhost</code>. An IP address does not work.
          </Callout>
        {/if}

        {#if available && showForm}
          <form class="flex flex-col gap-2" onsubmit={register}>
            <div class="flex items-end gap-2">
              <FormInput id="passkey-name" label="Passkey name" required maxlength={255} bind:value={name} />
              <button type="submit" class="button" disabled={loading} {...highlighted}>Create passkey</button>
              <button type="button" class="button" onclick={() => (showForm = false)}>Cancel</button>
            </div>
            {#if error}<p class="text-sm text-error">{error}</p>{/if}
          </form>
        {/if}

        {#each data.passkeys.list as passkey (passkey.id)}
          <div class="flex flex-wrap items-center justify-between gap-3 rounded-lg border border-neutral-200 p-3 dark:border-white/[0.07]">
            <div class="min-w-0">
              {#if editing === passkey.id}
                <form class="flex items-end gap-2" onsubmit={(e) => saveRename(e, passkey.id)}>
                  <FormInput id="passkey-edit-{passkey.id}" required maxlength={255} aria-label="Passkey name" bind:value={editName} />
                  <button type="submit" class="button">Save</button>
                </form>
              {:else}
                <div class="text-sm font-semibold text-black dark:text-fg">{passkey.name}</div>
              {/if}
              <p class="mt-1 text-xs text-neutral-500 dark:text-fg-dim">
                {passkey.authenticator ?? 'Passkey'}
                · Added {diffForHumans(passkey.created_at)}
                · {passkey.last_used_at ? `Last used ${diffForHumans(passkey.last_used_at)}` : 'Not used yet'}
              </p>
            </div>
            <div class="flex items-center gap-2">
              <button
                type="button"
                class="button"
                onclick={() => {
                  editName = passkey.name;
                  editing = editing === passkey.id ? null : passkey.id;
                }}
              >
                <span>{editing === passkey.id ? 'Cancel' : 'Rename'}</span>
              </button>
              <div class="relative h-auto max-w-full inline-flex w-auto">
                <button type="button" class="button button-error flex gap-2" onclick={() => (deleting = passkey)}>Delete</button>
              </div>
            </div>
          </div>
        {:else}
          {#if available && !showForm}
            <Empty size="sm" title="No passkeys" description="Add a passkey to sign in without your password." iconName="keys" />
          {/if}
        {/each}

        {#if renameError}<p class="text-sm text-error">{renameError}</p>{/if}
      </div>
    </section>
  </div>
{/if}

{#if deleting}
  <ConfirmModal
    title="Delete passkey?"
    actions={['You cannot sign in with this passkey after it is deleted.']}
    confirmWithText={data.confirm.text}
    confirmWithPassword={data.confirm.password}
    confirmationText={deleting.name}
    confirmationLabel="Enter the passkey name to confirm"
    shortConfirmationLabel="Passkey name"
    step3ButtonText="Delete passkey"
    onconfirm={remove}
    onclose={() => (deleting = null)}
  />
{/if}
