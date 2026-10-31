<script lang="ts">
  // Port of upstream livewire/security/cloud-init-script/show.blade.php (the page).
  import FormInput from '../components/FormInput.svelte';
  import FormTextarea from '../components/FormTextarea.svelte';
  import ConfirmModal from '../components/ConfirmModal.svelte';
  import UnsavedBar from '../components/UnsavedBar.svelte';
  import SecurityLayout from '../components/SecurityLayout.svelte';
  import SettingsSection from '../components/SettingsSection.svelte';
  import { ApiError } from '../lib/api';
  import { securityUi } from '../lib/securityApi';
  import { router } from '../lib/router.svelte';
  import { notify } from '../lib/toast.svelte';

  let { uuid }: { uuid: string } = $props();

  let loaded = $state(false);
  let name = $state('');
  let script = $state('');
  let savedName = $state('');
  let saved = $state('');
  let saving = $state(false);
  let error = $state<ApiError | null>(null);
  let deleting = $state(false);

  async function load() {
    const s = await securityUi.script(uuid);
    name = s.name;
    script = s.script;
    savedName = s.name;
    saved = JSON.stringify([name, script]);
    loaded = true;
    document.title = `${s.name} | Cloud-Init Scripts | coolify-rs`;
  }
  load();

  const dirty = $derived(loaded && JSON.stringify([name, script]) !== saved);

  async function save() {
    saving = true;
    error = null;
    try {
      const r = await securityUi.updateScript(uuid, { name, script });
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

  async function remove() {
    try {
      await securityUi.deleteScript(uuid);
      router.go('/security/cloud-init-scripts');
    } catch (e) {
      notify.failure(e instanceof ApiError ? e.message : String(e));
    }
  }
</script>

<div>
  {#if loaded}
    <SecurityLayout>
      {#snippet actions()}
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
        <SettingsSection title="General" description="Edit the reusable initialization script used during cloud server creation.">
          <div class="flex flex-col gap-4">
            <FormInput id="name" label="Script name" required bind:value={name} error={error?.field('name')} />
            <FormTextarea id="script" label="Script content" rows={18} monospace helper="Cloud-config YAML or another script accepted by your provider." required bind:value={script} error={error?.field('script')} />
          </div>
        </SettingsSection>
      </form>
    </SecurityLayout>
  {/if}
</div>

{#if deleting}
  <ConfirmModal
    title="Confirm Script Deletion?"
    actions={['This cloud-init script will be permanently deleted.', 'This action cannot be undone.']}
    confirmationText={savedName}
    confirmationLabel="Enter the script name to confirm deletion"
    shortConfirmationLabel="Script name"
    step2ButtonText="Delete script"
    onconfirm={remove}
    onclose={() => (deleting = false)}
  />
{/if}
