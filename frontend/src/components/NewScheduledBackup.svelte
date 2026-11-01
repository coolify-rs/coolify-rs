<script lang="ts">
  // `<x-modal-input title="New Scheduled Backup">` around livewire/project/database/create-scheduled-backup
  // on the storage row of a database (project/shared/storages/all.blade.php).
  import FormInput from './FormInput.svelte';
  import Modal from './Modal.svelte';
  import { ApiError } from '../lib/api';
  import { backupsUi } from '../lib/databaseApi';
  import { router } from '../lib/router.svelte';
  import { notify } from '../lib/toast.svelte';

  let { uuid, onclose }: { uuid: string; onclose: () => void } = $props();
  let frequency = $state('');
  let error = $state<ApiError | null>(null);

  async function submit(e: SubmitEvent) {
    e.preventDefault();
    error = null;
    try {
      const r = await backupsUi.create(uuid, frequency);
      frequency = '';
      onclose();
      if (r.backup_uuid) router.go(`${router.path.replace(/\/persistent-storage$/, '')}/backups/${r.backup_uuid}`);
    } catch (err) {
      error = err instanceof ApiError ? err : new ApiError(0, String(err));
      if (!Object.keys(error.errors).length) {
        onclose();
        notify.failure(error.message);
      }
    }
  }
</script>

<Modal title="New Scheduled Backup" {onclose}>
  <form class="application-settings-form flex w-full flex-col gap-4" onsubmit={submit}>
    <FormInput
      placeholder="0 0 * * * or daily"
      id="frequency"
      helper="You can use every_minute, hourly, daily, weekly, monthly, yearly or a cron expression."
      label="Frequency"
      required
      bind:value={frequency}
      error={error?.field('frequency')}
    />
    <div class="flex justify-end border-t border-neutral-200 pt-4 dark:border-white/[0.08]">
      <button type="submit" class="button button-highlighted">Add schedule</button>
    </div>
  </form>
</Modal>
