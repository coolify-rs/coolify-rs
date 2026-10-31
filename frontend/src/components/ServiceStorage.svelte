<script lang="ts">
  // Port of the service stack part of upstream livewire/project/service/storage.blade.php:
  // one settings card per compose service, with volumes, files and directories tabs.
  import { untrack } from 'svelte';
  import Empty from './Empty.svelte';
  import ServiceFileStorage from './ServiceFileStorage.svelte';
  import Storages from './shared/Storages.svelte';
  import SettingsSection from './SettingsSection.svelte';
  import { headline, type ServiceStoragePart } from '../lib/servicesApi';

  let {
    serviceUuid,
    part,
    canUpdate,
    onchanged,
  }: { serviceUuid: string; part: ServiceStoragePart; canUpdate: boolean; onchanged: () => Promise<void> } = $props();

  const hasVolumes = $derived(part.volume_count > 0);
  const hasFiles = $derived(part.files.length > 0);
  const hasDirectories = $derived(part.directories.length > 0);

  // `resolveDefaultTab`.
  let activeTab = $state<'volumes' | 'files' | 'directories'>(
    untrack(() => (part.volume_count > 0 ? 'volumes' : part.files.length > 0 ? 'files' : part.directories.length > 0 ? 'directories' : 'volumes')),
  );

  const base = 'h-7 rounded-md px-2.5 text-[12px] font-medium transition-colors disabled:cursor-not-allowed disabled:opacity-40';
  const on = 'bg-white text-black shadow-sm ring-1 ring-neutral-200 dark:bg-white/[0.09] dark:text-fg dark:ring-white/[0.08]';
  const off = 'text-neutral-500 hover:text-black dark:text-fg-faint dark:hover:text-fg';
</script>

<div class="flex flex-col gap-6">
  <SettingsSection
    id="storage-service-{part.uuid}"
    title={headline(part.name)}
    flush
    description="Volume mounts for this compose service. Compose-managed mounts are read-only in the dashboard."
  >
    {#snippet actions()}
      {#if hasVolumes || hasFiles || hasDirectories}
        <div class="inline-flex items-center gap-0.5 rounded-lg bg-neutral-100 p-1 dark:bg-white/[0.04]">
          <button type="button" disabled={!hasVolumes} class="{base} {activeTab === 'volumes' ? on : off}" onclick={() => (activeTab = 'volumes')}>
            Volumes ({part.volume_count})
          </button>
          <button type="button" disabled={!hasFiles} class="{base} {activeTab === 'files' ? on : off}" onclick={() => (activeTab = 'files')}>
            Files ({part.files.length})
          </button>
          <button type="button" disabled={!hasDirectories} class="{base} {activeTab === 'directories' ? on : off}" onclick={() => (activeTab = 'directories')}>
            Directories ({part.directories.length})
          </button>
        </div>
      {/if}
    {/snippet}

    {#if !hasVolumes && !hasFiles && !hasDirectories}
      <Empty size="sm" title="No storage found" description="No volumes, files, or directories are defined for this service." iconName="storages" />
    {:else if activeTab === 'volumes'}
      {#if hasVolumes}
        <Storages kind="service-{part.kind}" uuid={part.uuid} onchange={onchanged} volumesOnly />
      {:else}
        <Empty size="sm" title="No volumes configured" description="This service has no volume mounts." iconName="storages" />
      {/if}
    {:else if activeTab === 'files'}
      <div class="flex flex-col gap-4 p-4">
        {#each part.files as file (file.id)}
          <ServiceFileStorage {serviceUuid} {file} {canUpdate} {onchanged} />
        {:else}
          <Empty size="sm" title="No file mounts configured" description="This service has no file mounts." iconName="file" />
        {/each}
      </div>
    {:else}
      <div class="flex flex-col gap-4 p-4">
        {#each part.directories as file (file.id)}
          <ServiceFileStorage {serviceUuid} {file} {canUpdate} {onchanged} />
        {:else}
          <Empty size="sm" title="No directory mounts configured" description="This service has no directory mounts." iconName="folder" />
        {/each}
      </div>
    {/if}
  </SettingsSection>

  {#if part.external_volumes.length > 0}
    <SettingsSection
      id="external-volumes-{part.uuid}"
      title="External volumes"
      flush
      description="Docker volumes that the Docker Compose file declares with external: true. Coolify mounts them as written."
    >
      <div class="flex w-full flex-col">
        <div class="border-b border-neutral-200 px-4 py-3 text-[13px] leading-5 text-neutral-500 dark:border-white/[0.08] dark:text-fg-dim">
          Managed outside Coolify. Coolify never removes these volumes.
        </div>
        <div class="data-table w-full">
          <div class="data-table-header external-volumes-table-grid">
            <span>Volume</span>
            <span>Docker volume</span>
            <span>Mount path</span>
            <span>Service</span>
          </div>
          {#each part.external_volumes as v (v.service + v.key)}
            <div class="env-table-item">
              <div class="data-table-row external-volumes-table-grid text-[13px] text-neutral-700 dark:text-fg-dim">
                <div class="min-w-0">
                  <span class="volumes-mobile-label volumes-field-label">Volume</span>
                  <span class="block min-w-0 truncate font-medium text-neutral-950 dark:text-fg" title={v.key}>{v.key}</span>
                  <span class="block text-xs text-neutral-500 dark:text-fg-dim">External volume</span>
                </div>
                <div class="min-w-0">
                  <span class="volumes-mobile-label volumes-field-label">Docker volume</span>
                  <span class="block min-w-0 truncate text-neutral-950 dark:text-fg" title={v.dockerName}>{v.dockerName}</span>
                </div>
                <div class="min-w-0">
                  <span class="volumes-mobile-label volumes-field-label">Mount path</span>
                  {#each v.mountPaths as path (path)}
                    <span class="block min-w-0 truncate text-neutral-950 dark:text-fg" title={path}>{path}</span>
                  {/each}
                </div>
                <div class="min-w-0">
                  <span class="volumes-mobile-label volumes-field-label">Service</span>
                  <span class="block min-w-0 truncate" title={v.service}>{v.service}</span>
                </div>
              </div>
            </div>
          {/each}
        </div>
      </div>
    </SettingsSection>
  {/if}
</div>
