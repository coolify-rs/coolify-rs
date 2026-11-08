<script lang="ts">
  // Port of upstream livewire/storage/show.blade.php, livewire/storage/form.blade.php and
  // livewire/storage/resources.blade.php.
  import Reicon from '../components/Reicon.svelte';
  import Empty from '../components/Empty.svelte';
  import FormInput from '../components/FormInput.svelte';
  import Checkbox from '../components/Checkbox.svelte';
  import Listbox from '../components/Listbox.svelte';
  import UnsavedBar from '../components/UnsavedBar.svelte';
  import SettingsSection from '../components/SettingsSection.svelte';
  import StatusBadge from '../components/StatusBadge.svelte';
  import ConfirmModal from '../components/ConfirmModal.svelte';
  import DangerZone from '../components/DangerZone.svelte';
  import Callout from '../components/Callout.svelte';
  import EndpointInput from '../components/EndpointInput.svelte';
  import { ApiError } from '../lib/api';
  import { storagesUi, splitEndpoint, composeEndpoint, detectPathStyleEndpoint, type EndpointParts, type StorageDetails, type StorageForm, type StorageResources } from '../lib/storagesApi';
  import { router, link } from '../lib/router.svelte';
  import { notify } from '../lib/toast.svelte';
  import { page } from '../lib/page.svelte';

  let { uuid, section = 'general' }: { uuid: string; section?: string } = $props();

  let data = $state<StorageDetails | null>(null);
  let form = $state<StorageForm | null>(null);
  let parts = $state<EndpointParts>({ scheme: 'https', host: '', port: '', path: '' });
  let saved = $state('');
  let saving = $state(false);
  let usePathStyleEndpointChanged = $state(false);

  // svelte-ignore state_referenced_locally
  const base = `/storages/${uuid}`;

  function snapshot() {
    return JSON.stringify([form, parts]);
  }

  function applyForm(d: StorageDetails) {
    form = { name: d.name, description: d.description ?? '', endpoint: d.endpoint, bucket: d.bucket, region: d.region, key: d.key, secret: d.secret, use_path_style_endpoint: d.use_path_style_endpoint };
    parts = splitEndpoint(d.endpoint);
    usePathStyleEndpointChanged = d.use_path_style_endpoint_changed;
    saved = snapshot();
  }

  function setCrumbs(d: StorageDetails, usable: boolean) {
    page.resourceSwitcher = {
      title: 'S3 Storage',
      label: d.name,
      uuid: d.uuid,
      usable,
      items: d.storages.map((s) => ({ label: s.name, uuid: s.uuid, href: `/storages/${s.uuid}`, active: s.uuid === d.uuid })),
    };
  }

  async function load() {
    let d: StorageDetails;
    try {
      d = await storagesUi.show(uuid);
    } catch (e) {
      if (e instanceof ApiError && e.status === 404) {
        missing = true;
        return;
      }
      throw e;
    }
    data = d;
    document.title = `${d.name.length > 20 ? `${d.name.slice(0, 20)}...` : d.name} | S3 Storage | coolify-rs`;
    applyForm(d);
    setCrumbs(d, d.is_usable);
    page.navContext = {
      label: 'S3 Storage',
      backTitle: 'Back to Integrations',
      ariaLabel: 'S3 storage settings',
      name: d.name,
      groups: [
        [
          { label: 'General', icon: 'settings', href: base, active: section === 'general' },
          { label: 'Resources', icon: 'grid', href: `${base}/resources`, active: section === 'resources' },
        ],
        [{ label: 'Danger Zone', icon: 'danger', href: `${base}/danger`, active: section === 'danger' }],
      ],
    };
    if (section === 'resources') await loadResources();
  }
  let missing = $state(false);
  load();

  const dirty = $derived(!!form && snapshot() !== saved);
  const canUpdate = $derived(!!data?.can_update);
  const currentForm = (): StorageForm => ({ ...form!, endpoint: JSON.stringify(parts) !== JSON.stringify(splitEndpoint(form!.endpoint)) ? composeEndpoint(parts) : form!.endpoint });

  async function submit() {
    saving = true;
    try {
      const r = await storagesUi.update(uuid, currentForm());
      notify.success(r.message);
      const d = await storagesUi.show(uuid);
      data = d;
      applyForm(d);
      setCrumbs(d, true);
    } catch (e) {
      notify.failure(e instanceof ApiError ? e.message : String(e));
      // `$this->storage->refresh(); $this->syncData(false);`
      const d = await storagesUi.show(uuid);
      data = d;
      applyForm(d);
    } finally {
      saving = false;
    }
  }

  /** `updatedEndpointParts`: the URL style follows the provider until the user picks one. */
  function endpointChanged() {
    if (form && !usePathStyleEndpointChanged) form.use_path_style_endpoint = detectPathStyleEndpoint(composeEndpoint(parts));
  }

  async function testConnection() {
    // The round trip syncs `wire:model`, so the form is no longer dirty.
    saved = snapshot();
    try {
      const r = await storagesUi.test(uuid, currentForm());
      if (form) form.use_path_style_endpoint = r.use_path_style_endpoint;
      notify.ok(r.message, r.details);
      if (data) {
        data.is_usable = true;
        setCrumbs(data, true);
      }
    } catch (e) {
      const error = e instanceof ApiError ? e : new ApiError(0, String(e));
      notify.error(error.message, error.details);
      if (data) {
        data.is_usable = false;
        setCrumbs(data, false);
      }
    }
  }

  // Danger zone.
  let deleting = $state(false);
  async function remove() {
    try {
      await storagesUi.destroy(uuid);
      router.go('/storages');
    } catch (e) {
      notify.failure(e instanceof ApiError ? e.message : String(e));
    }
  }

  // Resources.
  let resources = $state<StorageResources | null>(null);
  let selected = $state<Record<string, number | string>>({});
  let search = $state('');
  let disabling = $state<{ id: number; volume: boolean } | null>(null);
  async function loadResources() {
    const r = await storagesUi.resources(uuid);
    resources = r;
    selected = {};
    for (const b of r.backups) selected[`d${b.id}`] = String(r.storage_id);
    for (const b of r.volume_backups) selected[`v${b.id}`] = String(r.storage_id);
  }
  const storageOptions = $derived(
    (resources?.storages ?? []).map((s) => ({
      value: String(s.id),
      label: s.name + (s.is_usable ? '' : ' (unusable)'),
      disabled: !s.is_usable,
    })),
  );
  const matchesSearch = (values: (string | null | undefined)[]) => {
    const q = search.toLowerCase();
    return search === '' || values.some((v) => (v ?? '').toLowerCase().includes(q));
  };

  async function moveBackup(id: number, volume: boolean) {
    try {
      const r = await storagesUi.move(uuid, id, volume, Number(selected[`${volume ? 'v' : 'd'}${id}`]));
      notify.ok(r.message, r.details);
      await loadResources();
    } catch (e) {
      const error = e instanceof ApiError ? e : new ApiError(0, String(e));
      notify.error(error.message, error.details);
    }
  }

  async function disableBackup() {
    if (!disabling) return;
    try {
      const r = await storagesUi.disable(uuid, disabling.id, disabling.volume);
      notify.ok(r.message, r.details);
      disabling = null;
      await loadResources();
    } catch (e) {
      notify.failure(e instanceof ApiError ? e.message : String(e));
    }
  }

  const rowGrid = 'grid min-w-[780px] grid-cols-[minmax(12rem,1fr)_9rem_7rem_minmax(12rem,1.2fr)_4.5rem]';
</script>

{#snippet storagePicker(key: string, id: number, volume: boolean)}
  <div class="flex items-end gap-2">
    <Listbox id="selected-{key}" options={storageOptions} bind:value={selected[key] as string} />
    <button type="button" class="button shrink-0" onclick={() => moveBackup(id, volume)}>Move</button>
  </div>
  <div class="flex justify-end">
    <button
      type="button"
      class="icon-button shrink-0 text-red-500 hover:text-red-600 dark:text-red-400 dark:hover:text-red-300"
      title="Disable storage"
      aria-label="Remove this storage from the backup schedule"
      onclick={() => (disabling = { id, volume })}
    >
      <Reicon name="trash" class="size-3.5" />
    </button>
  </div>
{/snippet}

{#if missing}
  <Empty title="Not found" description="This S3 storage does not exist." iconName="storages" />
{:else if data && form}
  <div>
    <!-- x-dashboard.navbar :mobileTitleOnly="true" -->
    <div class="flex flex-col">
      <header class="order-1 mb-4 flex min-w-0 items-start justify-between gap-4 lg:order-2 lg:hidden">
        <div class="min-w-0 flex-1">
          <div class="flex min-w-0 flex-wrap items-center gap-2">
            <h1 class="min-w-0 truncate text-[24px]! leading-7! font-semibold! tracking-tight!">{data.name}</h1>
          </div>
          <p class="mt-1 text-[13px] text-neutral-500 dark:text-fg-dim">{data.description || 'S3-compatible backup destination'}</p>
        </div>
      </header>
    </div>
    <section class="application-settings-workspace mt-4 w-full max-w-none lg:mt-0">
      <div class="min-w-0">
        {#if section === 'general'}
          <form
            class="application-settings-form"
            onsubmit={(e) => {
              e.preventDefault();
              submit();
            }}
          >
            <UnsavedBar {dirty} {saving} onsave={submit} onreset={() => data && applyForm(data)} />
            <SettingsSection title="General" description="S3-compatible destination used by database and volume backups.">
              {#snippet actions()}
                {#if canUpdate}
                  <button type="button" class="button" onclick={testConnection}>
                    <Reicon name="check-circle" class="size-3.5" />
                    Validate connection
                  </button>
                {/if}
              {/snippet}
              <div class="grid gap-4 lg:grid-cols-2">
                <FormInput disabled={!canUpdate} label="Name" id="name" bind:value={form.name} />
                <FormInput disabled={!canUpdate} label="Description" id="description" bind:value={form.description} />
                <div class="lg:col-span-2">
                  {#if canUpdate}
                    <!-- Sends the endpoint right away, so the URL style below follows the provider. -->
                    <div onchange={endpointChanged}>
                      <EndpointInput bind:parts />
                    </div>
                  {:else}
                    <FormInput id="endpoint" label="Endpoint" value={data.endpoint} disabled />
                  {/if}
                </div>
                <FormInput disabled={!canUpdate} required label="Bucket" id="bucket" bind:value={form.bucket} />
                <FormInput disabled={!canUpdate} required label="Region" id="region" bind:value={form.region} />
                {#if canUpdate}
                  <FormInput required type="password" label="Access key" id="key" bind:value={form.key} />
                  <FormInput required type="password" label="Secret key" id="secret" bind:value={form.secret} />
                {:else}
                  <FormInput id="key" label="Access key" disabled value="Hidden (only admins can view)" />
                  <FormInput id="secret" label="Secret key" disabled value="Hidden (only admins can view)" />
                {/if}
                <Checkbox id="usePathStyleEndpoint" label="Use path-style URLs" helper="Turn this off for providers that only accept virtual-hosted-style URLs (bucket.endpoint). coolify-rs turns it off for Alibaba Cloud OSS, Tencent Cloud COS, and Volcengine TOS, and when the provider rejects path-style URLs." disabled={!canUpdate} bind:checked={form.use_path_style_endpoint} onchange={() => (usePathStyleEndpointChanged = true)} />
              </div>
            </SettingsSection>
          </form>
        {:else if section === 'resources'}
          <div class="application-settings-form">
            <SettingsSection title="Backup schedules" description="Schedules currently writing backup data to this storage." flush>
              {#if resources}
                {#if resources.backups.length === 0 && resources.volume_backups.length === 0}
                  <Empty title="No backup schedules use this storage" description="Select this storage from a database or volume backup schedule to see it here." iconName="storages" size="sm" />
                {:else}
                  <div class="border-b border-neutral-200 p-3 dark:border-white/[0.08]">
                    <div class="relative w-full max-w-sm">
                      <Reicon name="search" class="pointer-events-none absolute top-1/2 left-2.5 z-10 size-3.5 -translate-y-1/2 text-neutral-400 dark:text-fg-faint" />
                      <input
                        bind:value={search}
                        type="search"
                        placeholder="Search backup schedules"
                        class="h-8! w-full rounded-lg! border-neutral-200! bg-white! py-0! pr-3! pl-8! text-[12px]! shadow-none! placeholder:text-neutral-400 focus:border-accent! focus:ring-0! dark:border-white/[0.08]! dark:bg-white/[0.035]! dark:text-fg! dark:placeholder:text-fg-faint"
                      />
                    </div>
                  </div>
                  <div class="overflow-x-auto">
                    <div class="{rowGrid} border-b border-neutral-200 bg-neutral-50 px-4 py-2.5 text-[11px] font-medium text-neutral-500 dark:border-white/[0.08] dark:bg-white/[0.05] dark:text-fg-faint">
                      <div>Backup target</div>
                      <div>Frequency</div>
                      <div>Status</div>
                      <div>Storage</div>
                      <div class="text-right">Actions</div>
                    </div>
                    {#each resources.backups as b (b.id)}
                      {#if matchesSearch([b.database.name, b.frequency])}
                        <div class="{rowGrid} min-h-14 items-center border-b border-neutral-200 px-4 py-2.5 text-[12px] last:border-b-0 dark:border-white/[0.07]">
                          <div class="min-w-0">
                            {#if b.database.link}
                              <a use:link class="truncate font-medium text-black hover:underline dark:text-fg" href={b.database.link}>{b.database.name}</a>
                            {:else}
                              <span class="truncate font-medium text-black dark:text-fg">{b.database.name}</span>
                            {/if}
                          </div>
                          <div>
                            {#if b.database.link && b.database.uuid}
                              <a use:link class="text-neutral-500 hover:underline dark:text-fg-dim" href="{b.database.link}/{b.uuid}">{b.frequency}</a>
                            {:else}
                              <span class="text-neutral-500 dark:text-fg-dim">{b.frequency}</span>
                            {/if}
                          </div>
                          <div class="flex items-center"><StatusBadge label={b.enabled ? 'Enabled' : 'Disabled'} type={b.enabled ? 'success' : 'warning'} /></div>
                          {@render storagePicker(`d${b.id}`, b.id, false)}
                        </div>
                      {/if}
                    {/each}
                    {#each resources.volume_backups as b (b.id)}
                      {#if matchesSearch([b.target.name, b.target.type, b.target.resource?.name, b.frequency])}
                        <div class="{rowGrid} min-h-14 items-center border-b border-neutral-200 px-4 py-2.5 text-[12px] last:border-b-0 dark:border-white/[0.07]">
                          <div class="min-w-0">
                            <div class="truncate font-medium text-black dark:text-fg">{b.target.name}</div>
                            <div class="truncate text-neutral-500 dark:text-fg-dim">
                              {b.target.type}{#if b.target.resource?.name}&nbsp;· {b.target.resource.name}{/if}
                            </div>
                          </div>
                          <div class="text-neutral-500 dark:text-fg-dim">{b.frequency}</div>
                          <div class="flex items-center"><StatusBadge label={b.enabled ? 'Enabled' : 'Disabled'} type={b.enabled ? 'success' : 'warning'} /></div>
                          {@render storagePicker(`v${b.id}`, b.id, true)}
                        </div>
                      {/if}
                    {/each}
                  </div>
                {/if}
              {/if}
            </SettingsSection>
          </div>
        {:else}
          <div class="application-settings-form">
            <SettingsSection id="storage-danger-section" title="Danger zone" description="Destructive actions for this S3 storage destination cannot be undone.">
              <DangerZone title="Delete storage">
                <p>
                  Permanently delete
                  <strong class="font-semibold text-black dark:text-fg">{data.name}</strong>
                  from coolify-rs. Existing objects in the bucket are not deleted.
                </p>
                <ul class="space-y-1 text-xs">
                  <li>• Backup schedules pointing at this storage will stop writing to S3.</li>
                  {#if data.backup_count > 0}
                    <li>• {data.backup_count} backup schedule(s) currently use this destination.</li>
                  {/if}
                  <li>• Bucket contents on the provider are left untouched.</li>
                  <li>• This storage destination cannot be restored from coolify-rs after deletion.</li>
                </ul>
                {#snippet action()}
                  {#if canUpdate}
                    <button type="button" class="button button-error" onclick={() => (deleting = true)}>Delete</button>
                  {:else}
                    <button type="button" class="button button-error" disabled title="You do not have permission to delete this storage.">Delete</button>
                  {/if}
                {/snippet}
              </DangerZone>
              {#if !canUpdate}
                <div class="mt-4">
                  <Callout type="danger" title="Insufficient permissions">Contact a team administrator if this storage must be deleted.</Callout>
                </div>
              {/if}
            </SettingsSection>
          </div>
        {/if}
      </div>
    </section>
  </div>
{/if}

{#if deleting && data}
  <ConfirmModal
    title="Confirm Storage Deletion?"
    actions={[
      'The selected storage location will be permanently deleted from coolify-rs.',
      ...(data.backup_count > 0 ? [`${data.backup_count} backup schedule(s) will stop saving to S3. Existing objects in this storage will not be deleted.`] : []),
    ]}
    confirmationText={data.name}
    confirmationLabel="Please confirm the execution of the actions by entering the Storage Name below"
    shortConfirmationLabel="Storage Name"
    step2ButtonText="Permanently Delete"
    onconfirm={remove}
    onclose={() => (deleting = false)}
  />
{/if}

{#if disabling}
  <ConfirmModal
    title="Remove this storage from the backup schedule?"
    actions={['Backups from this schedule will no longer be uploaded to this storage. S3 backup turns off when no other storage remains.']}
    confirmWithText={false}
    step2ButtonText="Remove storage"
    onconfirm={disableBackup}
    onclose={() => (disabling = null)}
  />
{/if}
