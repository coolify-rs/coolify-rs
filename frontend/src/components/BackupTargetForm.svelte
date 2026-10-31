<script lang="ts">
  // Port of upstream livewire/project/application/backup/create.blade.php, which
  // Application/Backup/Create and Service/VolumeBackup/Create share.
  import Empty from './Empty.svelte';
  import FormInput from './FormInput.svelte';
  import Listbox from './Listbox.svelte';
  import { ApiError } from '../lib/api';
  import { volumeBackupsUi, type BackupOwner, type BackupTargetOption } from '../lib/volumeBackupsApi';

  let {
    kind,
    uuid,
    targets,
    selectedTargetKey = null,
    oncreated,
  }: {
    kind: BackupOwner;
    uuid: string;
    targets: BackupTargetOption[];
    /** `$selectedTargetKey`: the target is locked to this volume. */
    selectedTargetKey?: string | null;
    oncreated: (backupUuid: string) => void;
  } = $props();

  const shown = $derived(selectedTargetKey ? targets.filter((t) => t.key === selectedTargetKey) : targets);
  let targetKey = $state<string | null>(null);
  let frequency = $state('daily');
  let error = $state<ApiError | null>(null);

  // `mount()`: the first target, and the frequency of a schedule it already has.
  $effect(() => {
    if (targetKey === null && shown.length) pick(shown[0].key);
  });
  function pick(key: string) {
    targetKey = key;
    const existing = shown.find((t) => t.key === key)?.frequency;
    if (existing) frequency = existing;
  }

  async function submit(e: Event) {
    e.preventDefault();
    error = null;
    try {
      const r = await volumeBackupsUi.create(kind, uuid, targetKey ?? '', frequency);
      oncreated(r.backup_uuid);
    } catch (err) {
      if (err instanceof ApiError) error = err;
      else throw err;
    }
  }
</script>

<form class="application-settings-form flex w-full flex-col gap-4" onsubmit={submit}>
  {#if !shown.length}
    <Empty size="sm" title="No backup targets" description="Add a persistent volume or directory mount before configuring a backup." iconName="storages" />
  {:else}
    <div class="grid gap-4 sm:grid-cols-2">
      <Listbox
        id="targetKey"
        label="Backup target"
        required
        disabled={!!selectedTargetKey}
        bind:value={() => targetKey ?? '', (v) => pick(String(v))}
        error={error?.field('targetKey')}
        options={shown.map((t) => ({ value: t.key, label: `${t.type}: ${t.name}` }))}
      />
      <FormInput
        id="frequency"
        placeholder="daily or 0 0 * * *"
        helper="Use every_minute, hourly, daily, weekly, monthly, yearly, or a cron expression."
        label="Frequency"
        required
        bind:value={frequency}
        error={error?.field('frequency')}
      />
    </div>
    <div class="mt-2 flex justify-end border-t border-neutral-200 pt-4 dark:border-white/[0.08]">
      <button type="submit" class="button button-highlighted">Create schedule</button>
    </div>
  {/if}
</form>
