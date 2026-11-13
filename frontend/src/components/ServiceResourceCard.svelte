<script lang="ts">
  // Port of upstream livewire/project/service/resource-card.blade.php: a compose
  // application or database as a table row or grid card, with its settings modal.
  import ConfirmModal from './ConfirmModal.svelte';
  import Modal from './Modal.svelte';
  import Reicon from './Reicon.svelte';
  import RowActions from './RowActions.svelte';
  import RowAction from './RowAction.svelte';
  import ServiceResourceForm from './ServiceResourceForm.svelte';
  import SkeletonForm from './SkeletonForm.svelte';
  import StatusBadge from './StatusBadge.svelte';
  import { ApiError } from '../lib/api';
  import { formatContainerStatus, headline, servicesApi, type ServicePart, type ServiceResourceDetails } from '../lib/servicesApi';
  import { link } from '../lib/router.svelte';
  import { notify } from '../lib/toast.svelte';

  let {
    serviceUuid,
    resource,
    isDatabase,
    viewMode,
    canUpdate,
    base,
    onrefresh,
  }: {
    serviceUuid: string;
    resource: ServicePart;
    isDatabase: boolean;
    viewMode: string;
    canUpdate: boolean;
    base: string;
    onrefresh: () => Promise<void>;
  } = $props();

  const isApplication = $derived(!isDatabase);
  const status = $derived(resource.status ?? '');
  const statusType = $derived<'success' | 'warning' | 'error'>(
    status.includes('running') && !status.includes('starting') ? 'success' : ['starting', 'restarting', 'degraded'].some((s) => status.includes(s)) ? 'warning' : 'error',
  );
  const statusLabel = $derived(formatContainerStatus(status));
  const name = $derived(headline(resource.human_name || resource.name));
  const restartLimit = $derived(
    status.startsWith('exited') && resource.restart_limit_reached ? Math.max(Number(resource.restart_count ?? 0), Number(resource.max_restart_count ?? 0)) : null,
  );
  const showBackups = $derived(isDatabase && (resource.backup_available || resource.is_migrated));
  const showDomains = $derived(isApplication && !!resource.fqdn && canUpdate);

  let modalOpen = $state(false);
  let details = $state<{ resource: ServiceResourceDetails; can_update: boolean } | null>(null);
  let confirmRestart = $state(false);

  async function openSettings() {
    modalOpen = true;
    details = null;
    try {
      details = await servicesApi.resource(serviceUuid, resource.uuid);
    } catch (e) {
      modalOpen = false;
      notify.error(e instanceof ApiError ? e.message : String(e));
    }
  }

  function rowClick(e: Event) {
    if ((e.target as HTMLElement).closest('a, button, [role=menu]')) return;
    openSettings();
  }

  async function restart() {
    try {
      notify.success((await servicesApi.restartResource(serviceUuid, resource.uuid)).message);
    } catch (e) {
      notify.error(e instanceof ApiError ? e.message : String(e));
    }
  }
</script>

{#snippet statusBadge()}
  {#if resource.is_removed_from_compose}
    <span class="inline-flex h-6 max-w-full items-center gap-1.5 whitespace-nowrap rounded-full border border-red-200 bg-red-50 px-2 text-xs font-medium leading-none text-red-700 dark:border-red-500/30 dark:bg-red-500/15 dark:text-red-300">
      <span class="size-1.5 shrink-0 rounded-full bg-red-500"></span>
      <span class="truncate">Not in Compose file</span>
    </span>
  {:else}
    <StatusBadge label={statusLabel} type={statusType} />
  {/if}
{/snippet}

{#snippet limitWarning()}
  {#if restartLimit !== null}
    <span title="Container has crashed and coolify-rs stopped it after {restartLimit} restart attempts."
      ><StatusBadge label="Restart limit reached" type="warning" /></span
    >
  {/if}
{/snippet}

{#snippet shortcuts()}
  {#if showBackups}
    <a class="icon-button" title="Service backups" aria-label="Service backups" use:link href="{base}/storage-backups">
      <Reicon name="database" class="size-4" />
    </a>
  {/if}
  {#if showDomains}
    <a class="icon-button" title="Manage domains" aria-label="Manage domains" use:link href="{base}/domains">
      <Reicon name="globe" class="size-4" />
    </a>
  {/if}
  <button type="button" class="icon-button" title="Resource settings" aria-label="Resource settings" onclick={openSettings}>
    <Reicon name="settings" class="size-4" />
  </button>
{/snippet}

{#if viewMode === 'grid'}
  <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
  <div
    onclick={rowClick}
    class="group flex min-w-0 flex-col overflow-hidden rounded-[10px] border border-neutral-200 bg-white transition-[border-color,background-color,box-shadow] hover:border-neutral-300 dark:border-white/[0.08] dark:bg-white/[0.03] dark:hover:border-white/[0.14]"
  >
    <div class="flex min-w-0 flex-1 items-start gap-3 p-4">
      <div class="flex size-9 shrink-0 items-center justify-center rounded-lg bg-neutral-100 text-neutral-500 dark:bg-white/[0.06] dark:text-fg-dim">
        <Reicon name={isDatabase ? 'database' : 'grid'} class="size-4" />
      </div>
      <div class="min-w-0 flex-1">
        <div class="flex min-w-0 items-start justify-between gap-3">
          <div class="min-w-0">
            <div class="truncate text-sm font-semibold text-black dark:text-fg">{name}</div>
            <div class="mt-0.5 truncate font-mono text-xs text-neutral-500 dark:text-fg-faint">{resource.image}</div>
          </div>
          {@render statusBadge()}
          {@render limitWarning()}
        </div>
        {#if resource.is_removed_from_compose}
          <p class="mt-2 text-xs leading-5 text-neutral-500 dark:text-fg-dim">This resource is not in the Docker Compose file. Add it back to use it again, or delete it in its settings.</p>
        {:else if resource.configuration_required}
          <div class="mt-2"><StatusBadge label="Configuration required" type="warning" /></div>
        {:else if resource.description}
          <p class="mt-2 line-clamp-2 text-xs leading-5 text-neutral-500 dark:text-fg-dim">{resource.description}</p>
        {/if}
      </div>
    </div>
    <div class="flex items-center justify-end gap-1 border-t border-neutral-200 bg-neutral-50 px-3 py-2 dark:border-white/[0.06] dark:bg-white/[0.02]">
      {@render shortcuts()}
      {#if status.includes('running') && canUpdate}
        <button type="button" class="button" onclick={() => (confirmRestart = true)}>Restart</button>
      {/if}
    </div>
  </div>
{:else}
  <div
    onclick={rowClick}
    onkeydown={(e) => e.key === 'Enter' && rowClick(e)}
    role="link"
    tabindex="0"
    aria-label="Open {name} settings"
    class="grid min-h-14 min-w-[48rem] cursor-pointer grid-cols-[minmax(14rem,1fr)_minmax(12rem,1fr)_12rem_5rem] items-center gap-3 border-b border-neutral-200 px-4 py-2.5 last:border-b-0 transition-colors hover:bg-neutral-50 dark:border-white/[0.06] dark:hover:bg-white/[0.03]"
  >
    <div class="flex min-w-0 items-center gap-3">
      <div class="flex size-8 shrink-0 items-center justify-center rounded-lg bg-neutral-100 text-neutral-500 dark:bg-white/[0.06] dark:text-fg-dim">
        <Reicon name={isDatabase ? 'database' : 'grid'} class="size-4" />
      </div>
      <div class="min-w-0">
        <div class="truncate text-[13px] font-semibold text-black dark:text-fg">{name}</div>
      </div>
    </div>
    <div class="truncate font-mono text-xs text-neutral-500 dark:text-fg-faint">{resource.image}</div>
    <div class="flex flex-wrap items-center gap-1" title={resource.is_removed_from_compose ? 'Not in the Docker Compose file. Add it back to use it again, or delete it in its settings.' : undefined}>
      {@render statusBadge()}
      {@render limitWarning()}
    </div>
    <div class="flex items-center justify-end">
      {#if showBackups || showDomains}
        <RowActions label="Resource actions">
          {#if showBackups}
            <RowAction href="{base}/storage-backups">Backups</RowAction>
          {/if}
          {#if showDomains}
            <RowAction href="{base}/domains">Domains</RowAction>
          {/if}
          <RowAction onclick={openSettings}>Settings</RowAction>
        </RowActions>
      {:else}
        <button type="button" class="icon-button" title="Resource settings" aria-label="Resource settings" onclick={openSettings}>
          <Reicon name="settings" class="size-4" />
        </button>
      {/if}
    </div>
  </div>
{/if}

{#if modalOpen}
  <Modal
    title={name}
    subtitle={isApplication ? 'Identity, image, and public access for this compose application.' : 'Identity, image, and public access for this compose database.'}
    isLarge
    focusFirstField={false}
    onclose={() => (modalOpen = false)}
  >
    {#if details}
      <ServiceResourceForm
        {serviceUuid}
        resource={details.resource}
        canUpdate={details.can_update}
        serviceBase={base}
        onsaved={async () => {
          await onrefresh();
        }}
        ondone={async () => {
          modalOpen = false;
          await onrefresh();
        }}
      />
    {:else}
      <SkeletonForm />
    {/if}
  </Modal>
{/if}

{#if confirmRestart}
  <ConfirmModal
    title={isApplication ? 'Confirm Service Application Restart?' : 'Confirm Service Database Restart?'}
    actions={[isApplication ? 'The selected service application will be unavailable during the restart.' : 'This service database will be unavailable during the restart.']}
    confirmWithText={false}
    confirmWithPassword={false}
    step2ButtonText={isApplication ? 'Restart Service Container' : 'Restart Database'}
    onconfirm={async () => {
      restart();
    }}
    onclose={() => (confirmRestart = false)}
  />
{/if}
