<script lang="ts">
  // Port of upstream livewire/project/service/import-backup.blade.php (Project/Service/ImportBackup).
  import { untrack } from 'svelte';
  import DatabaseImport from './DatabaseImport.svelte';
  import Empty from './Empty.svelte';
  import Forbidden from './Forbidden.svelte';
  import Listbox from './Listbox.svelte';
  import SettingsSection from './SettingsSection.svelte';
  import { ApiError, type DatabaseRecord } from '../lib/api';
  import { router } from '../lib/router.svelte';
  import { volumeBackupsUi } from '../lib/volumeBackupsApi';

  let { service, stack, base, canUpdate }: { service: string; stack: string | null; base: string; canUpdate: boolean } = $props();

  let databases = $state<{ uuid: string; name: string }[] | null>(null);
  let notFound = $state(false);
  let forbidden = $state(false);

  $effect(() => {
    untrack(async () => {
      try {
        const r = await volumeBackupsUi.importDatabases(service);
        // `mount()`: one compatible database goes straight to it, an unknown one is a 404.
        if (stack) {
          if (!r.databases.some((d) => d.uuid === stack)) notFound = true;
        } else if (r.databases.length === 1) {
          router.go(`${base}/import-backup/${r.databases[0].uuid}`, true);
          return;
        }
        databases = r.databases;
      } catch (e) {
        if (e instanceof ApiError && e.status === 403) forbidden = true;
        else throw e;
      }
    });
  });
</script>

{#if forbidden}
  <Forbidden />
{:else if notFound}
  <Empty title="Database not found" description="It may have been deleted, or it does not support imports." iconName="database" size="lg" />
{:else if databases}
  <div class="application-settings-form min-w-0 flex flex-col gap-6">
    {#if !databases.length}
      <SettingsSection title="Import Backup" description="Restore a backup into a database in this service.">
        <Empty title="No compatible databases" description="This service does not contain a database that supports backup imports." iconName="database" size="sm" />
      </SettingsSection>
    {:else}
      <SettingsSection title="Import Backup" description="Choose the database that should receive the backup.">
        <Listbox
          id="selectedDatabaseUuid"
          label="Database"
          required
          disabled={!canUpdate}
          bind:value={() => stack ?? '', (v) => router.go(`${base}/import-backup/${v}`)}
          options={databases.map((d) => ({ value: d.uuid, label: d.name }))}
        />
      </SettingsSection>

      {#if stack}
        {#key stack}
          <DatabaseImport record={{ uuid: stack } as DatabaseRecord} {canUpdate} />
        {/key}
      {/if}
    {/if}
  </div>
{/if}
