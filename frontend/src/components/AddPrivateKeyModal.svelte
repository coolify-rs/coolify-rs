<script lang="ts">
  // Port of upstream livewire/security/private-key/create.blade.php in its `x-modal-input`.
  import FormInput from './FormInput.svelte';
  import FormTextarea from './FormTextarea.svelte';
  import Modal from './Modal.svelte';
  import { api, ApiError, keysUi } from '../lib/api';
  import { notify } from '../lib/toast.svelte';

  let { title = 'Add Private Key Manually', onclose, oncreated }: { title?: string; onclose: () => void; oncreated: (key: { uuid: string }) => void } = $props();

  let newName = $state('');
  let newDescription = $state('');
  let newValue = $state('');
  let newPublic = $state('');
  let addError = $state<ApiError | null>(null);
  let addBusy = $state(false);

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
      const key = await api.post<{ uuid: string }>('/security/keys', { name: newName, description: newDescription || undefined, private_key: newValue });
      notify.ok('Private key created successfully.');
      oncreated(key);
    } catch (err) {
      addError = err instanceof ApiError ? err : new ApiError(0, String(err));
      if (!Object.keys(addError.errors).length) notify.error(addError.message);
    } finally {
      addBusy = false;
    }
  }
</script>

<Modal {title} {onclose}>
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
          <FormInput id="publicKey" readonly label="Public key" value={newPublic} helper="Copy this value to ~/.ssh/authorized_keys on the target server." />
        </div>
      </div>
      <div class="flex justify-end border-t border-neutral-200 pt-4 dark:border-white/[0.08]">
        <button type="submit" class="button button-highlighted" disabled={addBusy}>Continue</button>
      </div>
    </form>
  </div>
</Modal>
