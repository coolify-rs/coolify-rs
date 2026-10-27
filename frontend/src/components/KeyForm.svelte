<script lang="ts">
  // Port of upstream livewire/security/private-key/show.blade.php: the edit
  // form, used in the keys list modal (modalMode) and on its own page.
  import FormInput from './FormInput.svelte';
  import FormTextarea from './FormTextarea.svelte';
  import UnsavedBar from './UnsavedBar.svelte';
  import ConfirmModal from './ConfirmModal.svelte';
  import SettingsSection from './SettingsSection.svelte';
  import { api, ApiError, type PrivateKey } from '../lib/api';
  import { notify } from '../lib/toast.svelte';

  let {
    key,
    inUse,
    canEdit,
    modalMode = false,
    onsaved,
    ondeleted,
  }: {
    key: PrivateKey & { private_key?: string };
    inUse: boolean;
    canEdit: boolean;
    modalMode?: boolean;
    onsaved?: () => void;
    ondeleted?: () => void;
  } = $props();

  // The form edits a copy of the loaded key.
  // svelte-ignore state_referenced_locally
  let saved = $state({ name: key.name, description: key.description ?? '', value: key.private_key ?? '' });
  // svelte-ignore state_referenced_locally
  let name = $state(saved.name);
  // svelte-ignore state_referenced_locally
  let description = $state(saved.description);
  // svelte-ignore state_referenced_locally
  let value = $state(saved.value);
  let showPrivateKey = $state(false);
  let saving = $state(false);
  let error = $state<ApiError | null>(null);
  let deleting = $state(false);

  const dirty = $derived(name !== saved.name || description !== saved.description || value !== saved.value);
  const deleteDisabledReason = 'This private key is in use and cannot be deleted.';

  async function save(e?: Event) {
    e?.preventDefault();
    saving = true;
    error = null;
    try {
      const body: Record<string, unknown> = { name, description };
      if (value !== saved.value) body.private_key = value;
      await api.patch(`/security/keys/${encodeURIComponent(key.uuid)}`, body);
      saved = { name, description, value };
      notify.ok('Private key updated.');
      onsaved?.();
    } catch (err) {
      error = err instanceof ApiError ? err : new ApiError(0, String(err));
      if (!Object.keys(error.errors).length) notify.error(error.message);
    } finally {
      saving = false;
    }
  }

  async function remove() {
    await api.del(`/security/keys/${encodeURIComponent(key.uuid)}`);
    ondeleted?.();
  }
</script>

{#snippet fields(rows: number)}
  <div class="grid gap-4 lg:grid-cols-2">
    <FormInput id="name" label="Name" required disabled={!canEdit} bind:value={name} error={error?.field('name')} />
    <FormInput id="description" label="Description" disabled={!canEdit} bind:value={description} error={error?.field('description')} />
    <div class="lg:col-span-2">
      <FormInput
        id="public_key"
        label="Public key"
        readonly
        disabled={!canEdit}
        value={key.public_key}
        helper="Copy this value to ~/.ssh/authorized_keys on the target server."
      />
    </div>
    <div class="lg:col-span-2">
      <div class="mb-1.5 flex items-center justify-between gap-3">
        <label for="privateKeyValue" class="text-[13px] font-medium">Private key <span class="text-helper">*</span></label>
        {#if canEdit}
          <button
            type="button"
            class="text-[11px] font-medium text-coollabs hover:underline dark:text-brand-light"
            onclick={() => (showPrivateKey = !showPrivateKey)}>{showPrivateKey ? 'Hide editor' : 'Edit key'}</button
          >
        {/if}
      </div>
      {#if !showPrivateKey}
        <FormInput id="privateKeyValue" type="password" peek={false} required disabled {value} />
      {:else}
        <FormTextarea id="privateKeyValue" {rows} required monospace bind:value error={error?.field('private_key')} />
      {/if}
    </div>
  </div>
{/snippet}

{#snippet deleteButton()}
  <div class="relative h-auto max-w-full inline-flex w-auto">
    <span class="relative inline-flex" title={inUse ? deleteDisabledReason : undefined}>
      <button type="button" class="button button-error" disabled={inUse} onclick={() => (deleting = true)}>Delete</button>
    </span>
  </div>
{/snippet}

{#if modalMode}
  <div class="relative">
    <form onsubmit={save} class="flex flex-col gap-4">
      {@render fields(10)}
      <div class="flex items-center justify-between gap-2 border-t border-neutral-200 pt-4 dark:border-white/[0.08]">
        {#if canEdit}{@render deleteButton()}{:else}<span></span>{/if}
        <button type="submit" class="button" {...{ ishighlighted: '' }} disabled={!canEdit || saving}>Save changes</button>
      </div>
    </form>
  </div>
{:else}
  <div class="mb-3 flex min-h-8 flex-wrap items-center justify-end gap-2">
    {#if key.is_git_related}
      <span
        class="inline-flex h-6 max-w-full items-center gap-1.5 whitespace-nowrap rounded-full border border-neutral-200 bg-neutral-100 px-2 text-xs font-medium leading-none text-neutral-700 dark:border-white/[0.12] dark:bg-white/[0.07] dark:text-white"
      >
        <span class="relative flex size-1.5 shrink-0"><span class="relative inline-flex size-1.5 rounded-full bg-neutral-400 dark:bg-neutral-500"></span></span>
        <span class="truncate">Used by GitHub App</span>
      </span>
    {/if}
    {#if canEdit}{@render deleteButton()}{/if}
  </div>
  <form onsubmit={save} class="application-settings-form">
    <UnsavedBar
      {dirty}
      {saving}
      onsave={() => save()}
      onreset={() => ((name = saved.name), (description = saved.description), (value = saved.value), (error = null))}
    />
    <SettingsSection title="General">
      {@render fields(12)}
    </SettingsSection>
  </form>
{/if}

{#if deleting}
  <ConfirmModal
    title="Confirm Private Key Deletion?"
    actions={modalMode
      ? ['This private key will be permanently deleted.']
      : ['This private key will be permanently deleted.', 'Servers and Git sources using it will stop working.']}
    confirmationText={key.name}
    confirmationLabel={modalMode ? undefined : 'Enter the private key name to confirm deletion'}
    shortConfirmationLabel={modalMode ? undefined : 'Private key name'}
    step2ButtonText="Delete private key"
    onconfirm={remove}
    onclose={() => (deleting = false)}
  />
{/if}
