<script lang="ts">
  // Port of upstream livewire/security/cloud-provider-token/show.blade.php (the page).
  import Reicon from '../components/Reicon.svelte';
  import FormInput from '../components/FormInput.svelte';
  import ConfirmModal from '../components/ConfirmModal.svelte';
  import UnsavedBar from '../components/UnsavedBar.svelte';
  import SecurityLayout from '../components/SecurityLayout.svelte';
  import SettingsSection from '../components/SettingsSection.svelte';
  import { ApiError } from '../lib/api';
  import { securityUi, dateTimeOf, type CloudTokenRow } from '../lib/securityApi';
  import { router } from '../lib/router.svelte';
  import { notify } from '../lib/toast.svelte';

  let { uuid }: { uuid: string } = $props();

  let token = $state<CloudTokenRow | null>(null);
  let name = $state('');
  let description = $state('');
  let saved = $state('');
  let saving = $state(false);
  let error = $state<ApiError | null>(null);
  let deleting = $state(false);

  async function load() {
    const t = await securityUi.cloudToken(uuid);
    token = t;
    name = t.name;
    description = t.description ?? '';
    saved = JSON.stringify([name, description]);
    document.title = `${t.name} | Cloud Tokens | coolify-rs`;
  }
  load();

  const dirty = $derived(!!token && JSON.stringify([name, description]) !== saved);
  const providerName = (p: string) => (p === 'digitalocean' ? 'DigitalOcean' : p === 'vultr' ? 'Vultr' : p === 'hostinger' ? 'Hostinger' : 'Hetzner');

  async function save() {
    saving = true;
    error = null;
    try {
      const r = await securityUi.updateCloudToken(uuid, { name, description });
      notify.success(r.message);
      await load();
    } catch (e) {
      const err = e instanceof ApiError ? e : new ApiError(0, String(e));
      if (Object.keys(err.errors).length) error = err;
      else notify.failure(err.message);
    } finally {
      saving = false;
    }
  }

  async function validateToken() {
    try {
      notify.success((await securityUi.validateCloudToken(uuid)).message);
    } catch (e) {
      notify.failure(e instanceof ApiError ? e.message : String(e));
    }
  }

  async function remove() {
    try {
      await securityUi.deleteCloudToken(uuid);
      router.go('/security/cloud-tokens');
    } catch (e) {
      deleting = false;
      notify.failure(e instanceof ApiError ? e.message : String(e));
    }
  }
</script>

<div>
  {#if token}
    <SecurityLayout>
      {#snippet actions()}
        <button type="button" class="button" onclick={validateToken}>
          <Reicon name="check-circle" class="size-3.5" />
          Validate
        </button>
        <button type="button" class="button button-error" onclick={() => (deleting = true)}>Delete</button>
      {/snippet}
      <form
        class="application-settings-form"
        onsubmit={(e) => {
          e.preventDefault();
          save();
        }}
      >
        <UnsavedBar {dirty} {saving} onsave={save} onreset={load} />
        <SettingsSection title="General" description="Identity and provider details for this cloud API credential.">
          <div class="grid gap-4 lg:grid-cols-2">
            <FormInput id="name" label="Name" required bind:value={name} error={error?.field('name')} />
            <FormInput id="description" label="Description" bind:value={description} error={error?.field('description')} />
            <FormInput id="provider" readonly label="Provider" value={providerName(token.provider)} />
            <FormInput id="created" readonly label="Created" value={dateTimeOf(token.created_at)} />
          </div>
        </SettingsSection>
      </form>
    </SecurityLayout>
  {/if}
</div>

{#if deleting && token}
  <ConfirmModal
    title="Confirm Token Deletion?"
    actions={['This cloud provider token will be permanently deleted.', 'Servers using this token will need to be reconfigured.']}
    confirmationText={token.name}
    confirmationLabel="Enter the token name to confirm deletion"
    shortConfirmationLabel="Token name"
    step2ButtonText="Delete token"
    onconfirm={remove}
    onclose={() => (deleting = false)}
  />
{/if}
