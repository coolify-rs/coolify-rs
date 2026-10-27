<script lang="ts">
  // Port of upstream livewire/security/private-key/index.blade.php with
  // livewire/security/private-key/create.blade.php.
  import Reicon from '../components/Reicon.svelte';
  import Empty from '../components/Empty.svelte';
  import Modal from '../components/Modal.svelte';
  import FormInput from '../components/FormInput.svelte';
  import FormTextarea from '../components/FormTextarea.svelte';
  import ConfirmModal from '../components/ConfirmModal.svelte';
  import StatusBadge from '../components/StatusBadge.svelte';
  import SecurityLayout from '../components/SecurityLayout.svelte';
  import SettingsSection from '../components/SettingsSection.svelte';
  import KeyForm from '../components/KeyForm.svelte';
  import { api, keysUi, ApiError, type KeysPageData, type PrivateKey } from '../lib/api';
  import { session } from '../lib/session.svelte';
  import { notify } from '../lib/toast.svelte';

  let data = $state<KeysPageData | null>(null);
  let menuOpen = $state(false);
  let menuRoot: HTMLDivElement | undefined = $state();
  let cleaning = $state(false);
  let editing = $state<{ uuid: string; name: string; description: string } | null>(null);
  let editingKey = $state<(PrivateKey & { private_key?: string }) | null>(null);

  // Add manually.
  let adding = $state(false);
  let newName = $state('');
  let newDescription = $state('');
  let newValue = $state('');
  let newPublic = $state('');
  let addError = $state<ApiError | null>(null);
  let addBusy = $state(false);

  async function load() {
    data = await keysUi.list();
  }
  load();

  async function generate(type: 'ed25519' | 'rsa') {
    menuOpen = false;
    try {
      await keysUi.generate(type);
      notify.ok('Private key generated successfully.');
      await load();
    } catch (e) {
      notify.error(e instanceof ApiError ? e.message : String(e));
    }
  }

  async function cleanup() {
    await keysUi.cleanup();
    notify.ok('Unused keys have been cleaned up.');
    await load();
  }

  async function openEditor(k: KeysPageData['keys'][number]) {
    editing = { uuid: k.uuid, name: k.name, description: k.description ?? '' };
    editingKey = null;
    try {
      editingKey = await api.get<PrivateKey & { private_key?: string }>(`/security/keys/${encodeURIComponent(k.uuid)}`);
    } catch (e) {
      notify.error(e instanceof ApiError ? e.message : String(e));
      editing = null;
    }
  }

  let inspectTimer: ReturnType<typeof setTimeout> | undefined;
  function onValueInput() {
    clearTimeout(inspectTimer);
    inspectTimer = setTimeout(async () => {
      newPublic = newValue.trim() ? ((await keysUi.inspect(newValue).catch(() => null))?.public_key ?? '') : '';
    }, 200);
  }

  async function create(e: SubmitEvent) {
    e.preventDefault();
    addBusy = true;
    addError = null;
    try {
      await api.post('/security/keys', { name: newName, description: newDescription || undefined, private_key: newValue });
      notify.ok('Private key created successfully.');
      adding = false;
      await load();
    } catch (err) {
      addError = err instanceof ApiError ? err : new ApiError(0, String(err));
      if (!Object.keys(addError.errors).length) notify.error(addError.message);
    } finally {
      addBusy = false;
    }
  }

  const rowGrid = 'grid-cols-[minmax(0,1fr)_7rem_1.75rem] sm:grid-cols-[minmax(0,1.2fr)_minmax(0,1fr)_7rem_1.75rem]';
</script>

<svelte:window onclick={(e) => menuOpen && menuRoot && !menuRoot.contains(e.target as Node) && (menuOpen = false)} />

<div>
  <SecurityLayout>
    <SettingsSection title="Private keys" description="SSH keys used to connect servers and private repositories." flush>
      {#snippet actions()}
        {#if data?.can_create}
          <div class="relative h-auto max-w-full inline-flex w-auto">
            <div class="max-w-full inline-flex">
              <button type="button" class="button whitespace-nowrap text-error! hover:text-error! dark:text-error!" onclick={() => (cleaning = true)}>
                <span class="max-sm:hidden">Delete unused keys</span>
                <span class="sm:hidden">Delete unused</span>
              </button>
            </div>
          </div>
          <div class="relative" bind:this={menuRoot}>
            <button
              type="button"
              onclick={() => (menuOpen = !menuOpen)}
              class="button whitespace-nowrap button-highlighted"
              aria-haspopup="menu"
              aria-expanded={menuOpen}
            >
              <Reicon name="plus" class="size-3.5" />
              <span class="max-sm:hidden">New private key</span>
              <span class="sm:hidden">New key</span>
              <Reicon name="chevron-down" class="size-3 opacity-55" />
            </button>
            {#if menuOpen}
              <div role="menu" class="listbox-panel left-auto! right-0! z-[90]! w-52! min-w-52!">
                <button type="button" class="listbox-option justify-start! gap-2.5!" onclick={() => generate('ed25519')} role="menuitem">
                  <Reicon name="keys" class="size-3.5 shrink-0 opacity-70" />
                  Generate ED25519
                </button>
                <button type="button" class="listbox-option justify-start! gap-2.5!" onclick={() => generate('rsa')} role="menuitem">
                  <Reicon name="keys" class="size-3.5 shrink-0 opacity-70" />
                  Generate RSA
                </button>
                <button
                  type="button"
                  onclick={() => {
                    menuOpen = false;
                    adding = true;
                    newName = newDescription = newValue = newPublic = '';
                    addError = null;
                  }}
                  class="listbox-option justify-start! gap-2.5!"
                  role="menuitem"
                >
                  <Reicon name="plus" class="size-3.5 shrink-0 opacity-70" />
                  Add manually
                </button>
              </div>
            {/if}
          </div>
        {/if}
      {/snippet}

      {#if data && data.keys.length === 0}
        <Empty
          title="No private keys yet"
          description="Generate or add an SSH key to connect Coolify to servers and private repositories."
          iconName="keys"
        />
      {:else if data}
        <div>
          <div
            class="grid {rowGrid} items-center gap-3 border-b border-neutral-200 bg-neutral-50 px-4 py-2.5 text-[13px] font-medium text-neutral-500 dark:border-white/[0.08] dark:bg-white/[0.05] dark:text-fg-faint"
          >
            <div class="pl-11">Private key</div>
            <div class="hidden sm:block">Description</div>
            <div class="text-center">Status</div>
            <div class="w-7"></div>
          </div>
          {#each data.keys as key (key.uuid)}
            {#if key.can_view}
              <div class="border-b border-neutral-200 last:border-b-0 dark:border-white/[0.07]">
                <div
                  class="grid min-h-14 w-full {rowGrid} items-center gap-3 px-4 py-2.5 text-left transition-colors hover:bg-neutral-50 dark:hover:bg-white/[0.025]"
                >
                  <div class="flex min-w-0 items-center gap-3">
                    <div
                      class="flex size-8 shrink-0 items-center justify-center rounded-lg border border-neutral-200 bg-neutral-50 text-neutral-500 dark:border-white/[0.08] dark:bg-white/[0.04] dark:text-fg-dim"
                    >
                      <Reicon name="keys" class="size-4" />
                    </div>
                    <div class="min-w-0 flex-1">
                      <h3 class="truncate text-[13px]! leading-4! font-semibold! text-black dark:text-fg">{key.name}</h3>
                    </div>
                  </div>
                  <p class="hidden truncate text-[12px] text-neutral-500 sm:block dark:text-fg-dim">{key.description || '-'}</p>
                  <div class="flex justify-center">
                    {#if key.in_use}<StatusBadge label="In use" type="success" />{:else}<StatusBadge label="Unused" type="warning" />{/if}
                  </div>
                  <button type="button" class="icon-button" title="Edit private key" aria-label="Edit {key.name}" onclick={() => openEditor(key)}>
                    <Reicon name="settings" class="size-3.5" />
                  </button>
                </div>
              </div>
            {:else}
              <div
                class="grid min-h-14 cursor-not-allowed {rowGrid} items-center gap-3 border-b border-neutral-200 px-4 py-2.5 opacity-65 last:border-b-0 dark:border-white/[0.07]"
                title="You do not have permission to view this private key"
              >
                <div class="flex items-start gap-3">
                  <div
                    class="flex size-8 shrink-0 items-center justify-center rounded-lg border border-neutral-200 bg-white text-neutral-400 dark:border-white/[0.08] dark:bg-white/[0.05] dark:text-fg-faint"
                  >
                    <Reicon name="keys" class="size-4" />
                  </div>
                  <div class="min-w-0 flex-1">
                    <h3 class="truncate text-[13px]! leading-4! font-semibold! text-black dark:text-fg">{key.name}</h3>
                  </div>
                </div>
                <p class="hidden truncate text-[12px] text-neutral-500 sm:block dark:text-fg-dim">{key.description || '-'}</p>
                <div class="flex flex-wrap justify-center gap-2">
                  <StatusBadge label="View only" type="neutral" />
                  {#if !key.in_use}<StatusBadge label="Unused" type="warning" />{/if}
                </div>
                <span class="size-7"></span>
              </div>
            {/if}
          {/each}
        </div>
      {/if}
    </SettingsSection>
  </SecurityLayout>
</div>

{#if editing}
  <Modal title="Edit Private Key" onclose={() => (editing = null)}>
    {#if editingKey}
      <KeyForm
        key={editingKey}
        inUse={data?.keys.find((k) => k.uuid === editing?.uuid)?.in_use ?? false}
        canEdit={session.isAdmin}
        modalMode
        onsaved={() => ((editing = null), load())}
        ondeleted={() => ((editing = null), load())}
      />
    {:else}
      <div class="flex w-full flex-col gap-4" aria-label="Loading private key editor">
        <div class="grid gap-4 lg:grid-cols-2">
          <FormInput id="loading-name" label="Name" required value={editing.name} disabled />
          <FormInput id="loading-description" label="Description" value={editing.description} disabled />
        </div>
      </div>
    {/if}
  </Modal>
{/if}

{#if adding}
  <Modal title="Add Private Key Manually" onclose={() => (adding = false)}>
    <div class="application-settings-form">
      <form class="flex flex-col gap-4" onsubmit={create}>
        <div class="grid gap-4 lg:grid-cols-2">
          <FormInput id="name" label="Name" required bind:value={newName} error={addError?.field('name')} />
          <FormInput id="description" label="Description" bind:value={newDescription} error={addError?.field('description')} />
          <div class="lg:col-span-2">
            <FormTextarea
              id="value"
              rows={10}
              monospace
              placeholder="-----BEGIN OPENSSH PRIVATE KEY-----"
              label="Private key"
              required
              bind:value={newValue}
              oninput={onValueInput}
              error={addError?.field('private_key')}
            />
          </div>
          <div class="lg:col-span-2">
            <FormInput
              id="publicKey"
              readonly
              label="Public key"
              value={newPublic}
              helper="Copy this value to ~/.ssh/authorized_keys on the target server."
            />
          </div>
        </div>
        <div class="flex justify-end border-t border-neutral-200 pt-4 dark:border-white/[0.08]">
          <button type="submit" class="button button-highlighted" disabled={addBusy}>Continue</button>
        </div>
      </form>
    </div>
  </Modal>
{/if}

{#if cleaning}
  <ConfirmModal
    title="Confirm unused SSH Key Deletion?"
    actions={['All unused SSH keys (marked with unused) are permanently deleted.']}
    confirmWithText={false}
    onconfirm={cleanup}
    onclose={() => (cleaning = false)}
  />
{/if}
