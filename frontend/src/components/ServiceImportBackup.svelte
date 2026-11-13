<script lang="ts">
  // Port of upstream livewire/project/service/import-backup.blade.php (Project/Service/ImportBackup,
  // v4.7.5): the content of the "Import backup" modal on the service backups page. Pick a database,
  // then restore into it.
  import { untrack } from 'svelte';
  import DatabaseImport from './DatabaseImport.svelte';
  import Empty from './Empty.svelte';
  import Listbox from './Listbox.svelte';
  import { ApiError, type DatabaseRecord } from '../lib/api';
  import { notify } from '../lib/toast.svelte';
  import { volumeBackupsUi } from '../lib/volumeBackupsApi';

  let { service, canUpdate }: { service: string; canUpdate: boolean } = $props();

  let databases = $state<{ uuid: string; name: string }[] | null>(null);
  let selectedDatabaseUuid = $state('');

  $effect(() => {
    untrack(async () => {
      try {
        const r = await volumeBackupsUi.importDatabases(service);
        databases = r.databases;
        // `mount()`: a single compatible database is selected.
        if (r.databases.length === 1) selectedDatabaseUuid = r.databases[0].uuid;
      } catch (e) {
        notify.error(e instanceof ApiError ? e.message : String(e));
      }
    });
  });

  /** `selectedDatabase()`. */
  const selected = $derived(databases?.find((d) => d.uuid === selectedDatabaseUuid) ?? null);
</script>

{#if databases}
  <div class="flex flex-col gap-6">
    {#if !databases.length}
      <Empty title="No compatible databases" description="This service does not contain a database that supports backup imports." iconName="database" size="sm" />
    {:else}
      {#if databases.length > 1}
        <div class="max-w-sm">
          <Listbox
            id="selectedDatabaseUuid"
            label="Database"
            required
            disabled={!canUpdate}
            placeholder="Select a database"
            bind:value={selectedDatabaseUuid}
            options={databases.map((d) => ({ value: d.uuid, label: d.name }))}
          />
        </div>
      {/if}

      {#if selected}
        {#key selected.uuid}
          <DatabaseImport record={{ uuid: selected.uuid } as DatabaseRecord} {canUpdate} />
        {/key}
      {/if}
    {/if}
  </div>
{/if}
