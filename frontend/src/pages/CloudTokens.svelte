<script lang="ts">
  // Port of upstream livewire/security/cloud-tokens.blade.php,
  // livewire/security/cloud-provider-tokens.blade.php,
  // livewire/security/cloud-provider-token-form.blade.php and the modal of
  // livewire/security/cloud-provider-token/show.blade.php.
  import Reicon from '../components/Reicon.svelte';
  import Empty from '../components/Empty.svelte';
  import Modal from '../components/Modal.svelte';
  import FormInput from '../components/FormInput.svelte';
  import FormTextarea from '../components/FormTextarea.svelte';
  import Listbox from '../components/Listbox.svelte';
  import ConfirmModal from '../components/ConfirmModal.svelte';
  import SecurityLayout from '../components/SecurityLayout.svelte';
  import SettingsSection from '../components/SettingsSection.svelte';
  import { ApiError } from '../lib/api';
  import { securityUi, dateTimeOf, type CloudTokenRow } from '../lib/securityApi';
  import { notify } from '../lib/toast.svelte';

  document.title = 'Cloud Tokens | coolify-rs';

  let tokens = $state<CloudTokenRow[] | null>(null);
  async function load() {
    tokens = (await securityUi.cloudTokens()).tokens;
  }
  load();

  const providerName = (p: string) => (p === 'digitalocean' ? 'DigitalOcean' : p.charAt(0).toUpperCase() + p.slice(1));
  const consoleUrl = (p: string) =>
    p === 'hetzner'
      ? 'https://console.hetzner.com/projects'
      : p === 'vultr'
        ? 'https://console.vultr.com/user/apiaccess/'
        : p === 'hostinger'
          ? 'https://hpanel.hostinger.com/profile/api'
          : 'https://cloud.digitalocean.com/account/api/tokens';

  // New token.
  let adding = $state(false);
  let provider = $state('hetzner');
  let name = $state('');
  let token = $state('');
  let description = $state('');
  let addError = $state<ApiError | null>(null);

  async function addToken(e: SubmitEvent) {
    e.preventDefault();
    addError = null;
    try {
      const r = await securityUi.addCloudToken({ provider, token, name, description });
      token = '';
      name = '';
      description = '';
      adding = false;
      notify.success(r.message);
      await load();
    } catch (err) {
      const error = err instanceof ApiError ? err : new ApiError(0, String(err));
      if (Object.keys(error.errors).length) addError = error;
      else notify.failure(error.message);
    }
  }

  // Edit modal.
  let editing = $state<CloudTokenRow | null>(null);
  let editName = $state('');
  let editDescription = $state('');
  let editError = $state<ApiError | null>(null);
  let deleting = $state(false);

  function openEditor(t: CloudTokenRow) {
    editing = t;
    editName = t.name;
    editDescription = t.description ?? '';
    editError = null;
  }

  async function save(e: SubmitEvent) {
    e.preventDefault();
    if (!editing) return;
    editError = null;
    try {
      const r = await securityUi.updateCloudToken(editing.uuid, { name: editName, description: editDescription });
      notify.success(r.message);
      await load();
    } catch (err) {
      const error = err instanceof ApiError ? err : new ApiError(0, String(err));
      if (Object.keys(error.errors).length) editError = error;
      else notify.failure(error.message);
    }
  }

  async function validateToken() {
    if (!editing) return;
    try {
      notify.success((await securityUi.validateCloudToken(editing.uuid)).message);
    } catch (err) {
      notify.failure(err instanceof ApiError ? err.message : String(err));
    }
  }

  async function remove() {
    if (!editing) return;
    try {
      await securityUi.deleteCloudToken(editing.uuid);
      deleting = false;
      editing = null;
      await load();
    } catch (err) {
      deleting = false;
      notify.failure(err instanceof ApiError ? err.message : String(err));
    }
  }
</script>

<div>
  <SecurityLayout>
    <div class="application-settings-form">
      <SettingsSection title="Cloud tokens" description="Provider credentials used to provision cloud servers." flush>
        {#snippet actions()}
          <button type="button" class="button button-highlighted" onclick={() => (adding = true)}>
            <Reicon name="plus" class="size-3.5" />
            New token
          </button>
        {/snippet}
        {#if tokens}
          {#if tokens.length === 0}
            <Empty title="No cloud tokens" description="Add a provider token to provision new cloud servers." iconName="keys" size="sm" />
          {:else}
            <div>
              <div class="grid grid-cols-[minmax(0,1fr)_8rem_1.75rem] items-center gap-3 border-b border-neutral-200 bg-neutral-50 px-4 py-2.5 text-[13px] font-medium text-neutral-500 sm:grid-cols-[minmax(0,1fr)_8rem_minmax(0,1fr)_1.75rem] dark:border-white/[0.08] dark:bg-white/[0.05] dark:text-fg-faint">
                <div class="pl-11">Token</div>
                <div class="text-center">Provider</div>
                <div class="hidden sm:block">Description</div>
                <div class="w-7"></div>
              </div>
              {#each tokens as t (t.uuid)}
                <div class="border-b border-neutral-200 last:border-b-0 dark:border-white/[0.07]">
                  <div class="grid min-h-14 w-full grid-cols-[minmax(0,1fr)_8rem_1.75rem] items-center gap-3 px-4 py-2.5 text-left transition-colors hover:bg-neutral-50 sm:grid-cols-[minmax(0,1fr)_8rem_minmax(0,1fr)_1.75rem] dark:hover:bg-white/[0.025]">
                    <div class="flex min-w-0 items-center gap-3">
                      <div class="flex size-8 shrink-0 items-center justify-center rounded-lg border border-neutral-200 bg-neutral-50 text-neutral-500 dark:border-white/[0.08] dark:bg-white/[0.04] dark:text-fg-dim">
                        <Reicon name="keys" class="size-4" />
                      </div>
                      <div class="min-w-0 flex-1">
                        <h3 class="truncate text-[13px]! leading-4! font-semibold! text-black dark:text-fg">{t.name}</h3>
                      </div>
                    </div>
                    <div class="flex justify-center">
                      <span class="inline-flex rounded-full bg-neutral-100 px-2 py-0.5 text-[10px] font-medium text-neutral-600 dark:bg-white/[0.06] dark:text-fg-dim">
                        {providerName(t.provider)}
                      </span>
                    </div>
                    <p class="hidden truncate text-[12px] text-neutral-500 sm:block dark:text-fg-dim">{t.description || '-'}</p>
                    <button type="button" class="icon-button" title="Edit cloud token" aria-label="Edit {t.name}" onclick={() => openEditor(t)}>
                      <Reicon name="settings" class="size-3.5" />
                    </button>
                  </div>
                </div>
              {/each}
            </div>
          {/if}
        {/if}
      </SettingsSection>
    </div>
  </SecurityLayout>
</div>

{#if adding}
  <Modal title="New Cloud Token" onclose={() => (adding = false)}>
    <div class="w-full">
      <form class="application-settings-form flex w-full flex-col gap-4" onsubmit={addToken}>
        <Listbox
          required
          id="provider"
          label="Provider"
          bind:value={provider}
          options={[
            { value: 'hetzner', label: 'Hetzner' },
            { value: 'digitalocean', label: 'DigitalOcean' },
            { value: 'vultr', label: 'Vultr' },
            { value: 'hostinger', label: 'Hostinger' },
          ]}
        />
        <div class="rounded-lg border border-neutral-200 bg-neutral-50 p-3 text-[11px] leading-5 text-neutral-500 dark:border-white/[0.08] dark:bg-white/[0.05] dark:text-fg-dim">
          Create the token in the
          <a href={consoleUrl(provider)} target="_blank" class="font-medium text-coollabs hover:underline dark:text-brand-light">
            <span>{providerName(provider)} console</span>{' '}
          </a>.
        </div>
        <div class="grid gap-4 lg:grid-cols-2">
          <FormInput required id="name" label="Token name" placeholder="Production {providerName(provider)} token" bind:value={name} error={addError?.field('name')} />
          <FormInput required type="password" id="token" label="API token" placeholder="Paste the provider token" bind:value={token} error={addError?.field('token')} />
          <div class="lg:col-span-2">
            <FormTextarea id="description" label="Description" rows={3} placeholder="Optional notes about where this token is used" bind:value={description} error={addError?.field('description')} />
          </div>
        </div>
        <div class="flex justify-end border-t border-neutral-200 pt-4 dark:border-white/[0.08]">
          <button type="submit" class="button button-highlighted">Validate and add</button>
        </div>
      </form>
    </div>
  </Modal>
{/if}

{#if editing}
  <Modal title="Edit Cloud Token" onclose={() => (editing = null)}>
    <form onsubmit={save} class="flex flex-col gap-4">
      <div class="grid gap-4 lg:grid-cols-2">
        <FormInput id="name" label="Name" required bind:value={editName} error={editError?.field('name')} />
        <FormInput id="description" label="Description" bind:value={editDescription} error={editError?.field('description')} />
        <FormInput id="provider" readonly label="Provider" value={providerName(editing.provider)} />
        <FormInput id="created" readonly label="Created" value={dateTimeOf(editing.created_at)} />
      </div>
      <div class="flex items-center justify-between gap-2 border-t border-neutral-200 pt-4 dark:border-white/[0.08]">
        <div class="flex items-center gap-2">
          <button type="button" class="button button-error" onclick={() => (deleting = true)}>Delete</button>
          <button type="button" class="button" onclick={validateToken}>Validate</button>
        </div>
        <button type="submit" class="button" {...{ isHighlighted: '' }}>Save changes</button>
      </div>
    </form>
  </Modal>
{/if}

{#if deleting && editing}
  <ConfirmModal
    title="Confirm Token Deletion?"
    actions={['This cloud provider token will be permanently deleted.']}
    confirmationText={editing.name}
    step2ButtonText="Delete token"
    onconfirm={remove}
    onclose={() => (deleting = false)}
  />
{/if}
