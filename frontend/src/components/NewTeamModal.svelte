<script lang="ts">
  // Port of upstream livewire/team/create.blade.php in its modal-input.
  import Modal from './Modal.svelte';
  import FormInput from './FormInput.svelte';
  import { teamsUi } from '../lib/teamsApi';
  import { ApiError } from '../lib/api';
  import { session } from '../lib/session.svelte';
  import { router } from '../lib/router.svelte';
  import { notify } from '../lib/toast.svelte';

  let { onclose }: { onclose: () => void } = $props();

  let name = $state('');
  let description = $state('');
  let busy = $state(false);
  let error = $state<ApiError | null>(null);

  async function submit(e: SubmitEvent) {
    e.preventDefault();
    if (busy) return;
    busy = true;
    error = null;
    try {
      await teamsUi.create(name, description);
      await session.refresh();
      onclose();
      if (router.path === '/team') location.reload();
      else router.go('/team');
    } catch (err) {
      if (err instanceof ApiError) error = err;
      notify.failure(err instanceof ApiError ? err.message : String(err));
    } finally {
      busy = false;
    }
  }
</script>

<Modal title="New Team" {onclose}>
  <form class="application-settings-form flex w-full flex-col gap-4" onsubmit={submit}>
    <FormInput id="name" label="Name" required bind:value={name} error={error?.field('name')} />
    <FormInput id="description" label="Description" bind:value={description} error={error?.field('description')} />
    <div class="flex justify-end">
      <button type="submit" class="button button-highlighted" disabled={busy}>Create team</button>
    </div>
  </form>
</Modal>
