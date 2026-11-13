<script lang="ts">
  // Port of upstream livewire/security/private-key/show.blade.php with
  // partials/key-field.blade.php (v4.7.5): the editor in the keys list modal. The stored key is
  // never loaded with the form; "Reveal key" asks for the password, "Replace key" takes a new one.
  import { untrack } from 'svelte';
  import FormInput from './FormInput.svelte';
  import FormTextarea from './FormTextarea.svelte';
  import ConfirmModal from './ConfirmModal.svelte';
  import KeyRotate from './KeyRotate.svelte';
  import Loading from './Loading.svelte';
  import { api, ApiError } from '../lib/api';
  import { link } from '../lib/router.svelte';
  import { privateKeyUi, type PrivateKeyEditor } from '../lib/privateKeyApi';
  import { session } from '../lib/session.svelte';
  import { notify } from '../lib/toast.svelte';

  let {
    key: initial,
    onsaved,
    ondeleted,
  }: {
    key: PrivateKeyEditor;
    onsaved?: () => void;
    ondeleted?: () => void;
  } = $props();

  // `syncData(false)`: the form edits a copy of the loaded key.
  let key = $state<PrivateKeyEditor>(untrack(() => initial));
  let name = $state(untrack(() => initial.name));
  let description = $state(untrack(() => initial.description ?? ''));
  /** A new private key that replaces the stored key. */
  let newPrivateKey = $state('');
  let saving = $state(false);
  let error = $state<ApiError | null>(null);
  let confirmDelete = $state(false);
  let deleting = $state(false);

  // key-field.blade.php's Alpine state.
  let replacing = $state(false);
  let askPassword = $state(false);
  let password = $state('');
  let revealed = $state('');
  let revealing = $state(false);
  let skipPassword = $state(untrack(() => session.info?.user.skip_password_confirmation ?? false));

  let usageSearch = $state('');
  let searchTimer: ReturnType<typeof setTimeout> | undefined;

  const canUpdate = $derived(key.can_update);
  const deleteDisabledReason = 'This private key is in use and cannot be deleted.';
  const total = $derived(key.counts.reduce((sum, c) => sum + c.count, 0));
  /** `$count.' '.str($type)->plural($count)->lower()`. */
  const countsText = $derived(key.counts.map((c) => `${c.count} ${(c.count === 1 ? c.type : `${c.type}s`).toLowerCase()}`).join(' · '));

  /** `$refresh`: the "Used by" list for the search, and the key's state. */
  async function refresh() {
    try {
      key = await privateKeyUi.show(key.uuid, usageSearch.trim());
    } catch (e) {
      notify.error(e instanceof ApiError ? e.message : String(e));
    }
  }

  /** `wire:model.live.debounce.300ms="usageSearch"`. */
  function searchUsages() {
    clearTimeout(searchTimer);
    searchTimer = setTimeout(refresh, 300);
  }

  /** `changePrivateKey()`. */
  async function save(e?: Event) {
    e?.preventDefault();
    saving = true;
    error = null;
    try {
      const body: Record<string, unknown> = { name, description };
      if (newPrivateKey.trim() !== '') body.private_key = newPrivateKey;
      await api.patch(`/security/keys/${encodeURIComponent(key.uuid)}`, body);
      newPrivateKey = '';
      replacing = false;
      notify.ok('Private key updated.');
      onsaved?.();
    } catch (err) {
      error = err instanceof ApiError ? err : new ApiError(0, String(err));
      if (!Object.keys(error.errors).length) notify.error(error.message);
    } finally {
      saving = false;
    }
  }

  /** `delete()`. */
  async function remove() {
    deleting = true;
    try {
      await api.del(`/security/keys/${encodeURIComponent(key.uuid)}`);
      ondeleted?.();
    } finally {
      deleting = false;
    }
  }

  /** `reveal()` of key-field.blade.php: `revealPrivateKey($password)`. */
  async function reveal() {
    if (replacing) return;
    if (!skipPassword && !password) {
      askPassword = true;
      return;
    }
    const sentPassword = password;
    revealing = true;
    let value: string | null = null;
    try {
      value = (await privateKeyUi.reveal(key.uuid, password)).private_key;
    } catch (e) {
      notify.error(e instanceof ApiError ? e.message : String(e));
    }
    revealing = false;
    password = '';
    if (value && !replacing) {
      revealed = value;
      askPassword = false;
    } else if (!value && !sentPassword) {
      skipPassword = false;
      askPassword = true;
    }
  }

  function hide() {
    revealed = '';
    askPassword = false;
    password = '';
  }

  // `x-on:livewire:navigating.document="hide()"`: the revealed key does not outlive the editor.
  $effect(() => () => hide());
</script>

<div class="relative">
  <form onsubmit={save} class="flex flex-col gap-4 {deleting ? 'pointer-events-none opacity-50' : ''}">
    <div class="grid gap-4 lg:grid-cols-2">
      <FormInput id="name" label="Name" required disabled={!canUpdate} bind:value={name} error={error?.field('name')} />
      <FormInput id="description" label="Description" disabled={!canUpdate} bind:value={description} error={error?.field('description')} />
      <div class="lg:col-span-2">
        <FormInput
          id="public_key"
          label="Public key"
          readonly
          disabled={!canUpdate}
          value={key.public_key}
          helper="Copy this value to ~/.ssh/authorized_keys on the target server."
        />
      </div>
      <div class="lg:col-span-2">
        <FormInput id="fingerprint" label="Fingerprint" readonly value={key.fingerprint ?? ''} />
      </div>
      <div class="lg:col-span-2">
        <div class="mb-1.5 flex items-center justify-between gap-3">
          <!-- svelte-ignore a11y_label_has_associated_control -->
          <label class="text-[13px] font-medium">Private key</label>
          {#if canUpdate}
            <div class="flex items-center gap-3">
              <button
                type="button"
                class="text-[11px] font-medium text-coollabs hover:underline disabled:cursor-not-allowed disabled:no-underline disabled:opacity-50 dark:text-brand-light"
                onclick={() => (revealed || askPassword ? hide() : reveal())}
                disabled={replacing}>{revealed || askPassword ? 'Hide key' : 'Reveal key'}</button
              >
              <button
                type="button"
                class="text-[11px] font-medium text-coollabs hover:underline dark:text-brand-light"
                onclick={() => {
                  replacing = !replacing;
                  if (replacing) hide();
                  else newPrivateKey = '';
                }}>{replacing ? 'Cancel replace' : 'Replace key'}</button
              >
            </div>
          {/if}
        </div>
        {#if !replacing && !revealed && !askPassword}
          <p class="text-xs text-neutral-500 dark:text-fg-dim">
            The private key is stored encrypted and is not shown. Use the public key and the fingerprint to identify it.
          </p>
        {/if}
        {#if canUpdate}
          {#if askPassword && !revealed}
            <div class="flex items-end gap-2">
              <div class="flex-1">
                <!-- svelte-ignore a11y_label_has_associated_control -->
                <label class="text-[13px] font-medium">Enter your password to reveal the private key</label>
                <input
                  type="password"
                  class="w-full input"
                  bind:value={password}
                  autocomplete="current-password"
                  onkeydown={(e) => {
                    if (e.key === 'Enter') {
                      e.preventDefault();
                      reveal();
                    }
                  }}
                  placeholder="Enter your password"
                />
              </div>
              <button type="button" class="button" onclick={reveal} disabled={!password || revealing}>Reveal</button>
            </div>
          {/if}
          {#if revealed}
            <div>
              <pre class="input h-auto! overflow-x-auto whitespace-pre font-mono text-xs">{revealed}</pre>
            </div>
          {/if}
          {#if replacing}
            <div class="mt-2">
              <FormTextarea
                id="newPrivateKey"
                rows={10}
                monospace
                placeholder="Paste the new private key. Leave empty to keep the stored key."
                bind:value={newPrivateKey}
                error={error?.field('private_key')}
              />
            </div>
          {/if}
        {/if}
      </div>
      <div class="lg:col-span-2">
        <div class="mb-1.5 flex items-baseline justify-between gap-3">
          <!-- svelte-ignore a11y_label_has_associated_control -->
          <label class="text-[13px] font-medium">Used by</label>
          {#if key.counts.length}
            <span class="text-xs text-neutral-500 dark:text-fg-dim">{countsText}</span>
          {/if}
        </div>
        {#if !key.counts.length}
          <p class="text-xs text-neutral-500 dark:text-fg-dim">Nothing uses this key.</p>
        {:else}
          {#if total > 5}
            <input
              type="search"
              class="input mb-2"
              placeholder="Search by name"
              aria-label="Search where this key is used"
              bind:value={usageSearch}
              oninput={searchUsages}
              onkeydown={(e) => e.key === 'Enter' && e.preventDefault()}
            />
          {/if}
          <ul class="max-h-48 divide-y divide-neutral-200 overflow-y-auto rounded-md border border-neutral-200 text-sm dark:divide-white/[0.06] dark:border-white/[0.08]">
            {#each key.rows as usage (`${usage.type}-${usage.url ?? usage.name}`)}
              <li class="flex items-center gap-2 px-3 py-1">
                <span class="w-20 shrink-0 text-xs text-neutral-500 dark:text-fg-dim">{usage.type}</span>
                {#if usage.url}
                  <a class="truncate underline" href={usage.url} use:link>{usage.name}</a>
                {:else}
                  <span class="truncate">{usage.name}</span>
                {/if}
              </li>
            {:else}
              <li class="px-3 py-1 text-xs text-neutral-500 dark:text-fg-dim">No match.</li>
            {/each}
            {#if key.matches > key.rows.length}
              <li class="px-3 py-1 text-xs text-neutral-500 dark:text-fg-dim">and {key.matches - key.rows.length} more</li>
            {/if}
          </ul>
        {/if}
      </div>
    </div>
    <div class="flex items-center justify-between gap-2 border-t border-neutral-200 pt-4 dark:border-white/[0.08]">
      {#if key.can_delete}
        <div class="relative h-auto max-w-full inline-flex w-auto">
          <span class="relative inline-flex" title={key.in_use ? deleteDisabledReason : undefined}>
            <button type="button" class="button button-error" disabled={key.in_use} onclick={() => (confirmDelete = true)}>Delete</button>
          </span>
        </div>
      {:else}
        <span></span>
      {/if}
      <div class="flex items-center gap-2">
        {#if canUpdate}
          <KeyRotate uuid={key.uuid} canDelete={key.can_delete} onrotated={refresh} ondeleteoldkey={() => (confirmDelete = true)} />
        {/if}
        <button type="submit" class="button" {...{ ishighlighted: '' }} disabled={!canUpdate || saving}>Save changes</button>
      </div>
    </div>
  </form>
  {#if deleting}
    <div class="absolute inset-0 z-10 flex items-center justify-center rounded-lg bg-white/50 dark:bg-black/40">
      <Loading text="Deleting private key..." />
    </div>
  {/if}
</div>

{#if confirmDelete}
  <ConfirmModal
    title="Confirm Private Key Deletion?"
    actions={['This private key will be permanently deleted.', 'coolify-rs does not remove its public key from servers. To remove it, rotate the key before you delete it.']}
    confirmationText={key.name}
    step2ButtonText="Delete private key"
    onconfirm={remove}
    onclose={() => (confirmDelete = false)}
  />
{/if}
