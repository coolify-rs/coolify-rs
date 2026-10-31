<script lang="ts">
  // Port of upstream livewire/storage/index.blade.php and livewire/storage/create.blade.php.
  import Reicon from '../components/Reicon.svelte';
  import Empty from '../components/Empty.svelte';
  import Modal from '../components/Modal.svelte';
  import ViewToggle from '../components/ViewToggle.svelte';
  import StatusBadge from '../components/StatusBadge.svelte';
  import FormInput from '../components/FormInput.svelte';
  import EndpointInput from '../components/EndpointInput.svelte';
  import { ApiError } from '../lib/api';
  import { storagesUi, composeEndpoint, type EndpointParts, type StorageRow } from '../lib/storagesApi';
  import { link, router } from '../lib/router.svelte';
  import { notify } from '../lib/toast.svelte';

  document.title = 'Storages | coolify-rs';

  let storages = $state<StorageRow[] | null>(null);
  let canCreate = $state(false);
  storagesUi.list().then((d) => {
    storages = d.storages;
    canCreate = d.can_create;
  });

  const describe = (s: StorageRow) => s.description || 'S3-compatible storage';
  const status = (s: StorageRow) => (s.is_usable ? 'Connected' : 'Not usable');
  let search = $state('');
  let viewMode = $state(readView());
  function readView(): string {
    try {
      return localStorage.getItem('coolify-s3-storages-view') || 'table';
    } catch {
      return 'table';
    }
  }
  function setView(m: string) {
    viewMode = m;
    try {
      localStorage.setItem('coolify-s3-storages-view', m);
    } catch {
      // Storage unavailable: the choice lasts for this page only.
    }
  }
  const matches = (s: StorageRow) => {
    const q = search.trim().toLowerCase();
    return !q || [s.name, describe(s), status(s)].some((v) => v.toLowerCase().includes(q));
  };
  const filtered = $derived((storages ?? []).filter(matches));

  // New storage.
  let creating = $state(false);
  let name = $state('');
  let description = $state('');
  let parts = $state<EndpointParts>({ scheme: 'https', host: '', port: '', path: '' });
  let bucket = $state('');
  let region = $state('us-east-1');
  let key = $state('');
  let secret = $state('');
  let busy = $state(false);

  async function submit(e: SubmitEvent) {
    e.preventDefault();
    busy = true;
    try {
      const r = await storagesUi.create({ name, description, endpoint: composeEndpoint(parts), bucket, region, key, secret });
      router.go(`/storages/${r.uuid}`);
    } catch (err) {
      const error = err instanceof ApiError ? err : new ApiError(0, String(err));
      notify.error(error.message, error.details);
    } finally {
      busy = false;
    }
  }
  const grid = 'grid min-w-[620px] grid-cols-[minmax(0,1fr)_minmax(10rem,.8fr)_9rem]';
</script>

<div class="application-settings-form w-full">
  <header class="mb-5 flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
    <div class="min-w-0">
      <h1 class="truncate text-[24px]! leading-7! font-semibold! tracking-tight!">S3 Storage</h1>
      <p class="mt-1 text-[13px] text-neutral-500 dark:text-fg-dim">
        {#if storages}
          {storages.length} {storages.length === 1 ? 'storage destination' : 'storage destinations'} for backups
        {:else}
          &nbsp;
        {/if}
      </p>
    </div>
    {#if canCreate}
      <div class="w-fit shrink-0">
        <button type="button" class="button button-highlighted" onclick={() => (creating = true)}>
          <Reicon name="plus" class="size-3.5" />
          New storage
        </button>
      </div>
    {/if}
  </header>

  {#if storages}
    {#if storages.length === 0}
      <Empty title="No S3 storage yet" description="Add an S3-compatible destination to store backups outside your servers." iconName="storages" />
    {:else}
      <div class="mb-4 flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
        <div class="w-full sm:max-w-sm">
          <div class="relative w-full">
            <Reicon name="search" class="pointer-events-none absolute top-1/2 left-2.5 z-10 size-3.5 -translate-y-1/2 text-neutral-400 dark:text-fg-faint" />
            <input
              bind:value={search}
              type="search"
              placeholder="Search S3 storages"
              class="input w-full pl-8! h-8! rounded-lg! border-neutral-200! bg-white! py-0! pr-8! text-[12px]! shadow-none! placeholder:text-neutral-400 focus:border-accent! focus:ring-0! dark:border-white/[0.08]! dark:bg-white/[0.035]! dark:text-fg! dark:placeholder:text-fg-faint"
            />
          </div>
        </div>
        <ViewToggle bind:mode={() => viewMode, setView} />
      </div>
      {#if viewMode === 'grid'}
        <div class="grid grid-cols-1 gap-3 sm:grid-cols-2 lg:grid-cols-4">
          {#each filtered as s (s.uuid)}
            <a
              use:link
              href="/storages/{s.uuid}"
              class="group flex min-h-28 flex-col rounded-xl border border-neutral-200 bg-white p-3 shadow-sm transition-all hover:-translate-y-px hover:border-neutral-300 hover:no-underline hover:shadow-md dark:border-white/[0.08] dark:bg-white/[0.05] dark:hover:border-white/[0.14]"
            >
              <div class="flex items-start gap-3">
                <div class="flex size-8 shrink-0 items-center justify-center rounded-lg border border-neutral-200 bg-neutral-50 text-neutral-500 dark:border-white/[0.08] dark:bg-white/[0.04] dark:text-fg-dim">
                  <Reicon name="storages" class="size-4" />
                </div>
                <div class="min-w-0 flex-1">
                  <h2 class="truncate text-[13px]! leading-4! font-semibold! text-black dark:text-fg">{s.name}</h2>
                  <p class="mt-0.5 truncate text-[11px] text-neutral-500 dark:text-fg-faint">{describe(s)}</p>
                </div>
              </div>
              <div class="mt-auto pt-4">
                <StatusBadge label={status(s)} type={s.is_usable ? 'success' : 'error'} />
              </div>
            </a>
          {/each}
        </div>
      {:else}
        <div class="overflow-x-auto rounded-xl border border-neutral-200 bg-white shadow-sm dark:border-white/[0.08] dark:bg-white/[0.05]">
          <div class="{grid} border-b border-neutral-200 bg-neutral-50 px-4 py-2.5 text-[11px] font-medium text-neutral-500 dark:border-white/[0.08] dark:bg-white/[0.05] dark:text-fg-faint">
            <div>Storage</div>
            <div>Description</div>
            <div>Status</div>
          </div>
          {#each filtered as s (s.uuid)}
            <a
              use:link
              href="/storages/{s.uuid}"
              class="{grid} min-h-14 items-center border-b border-neutral-200 px-4 py-2.5 text-[12px] transition-colors last:border-b-0 hover:bg-neutral-50 hover:no-underline dark:border-white/[0.07] dark:hover:bg-white/[0.025]"
            >
              <div class="truncate font-semibold text-black dark:text-fg">{s.name}</div>
              <div class="truncate text-neutral-500 dark:text-fg-dim">{describe(s)}</div>
              <div><StatusBadge label={status(s)} type={s.is_usable ? 'success' : 'error'} /></div>
            </a>
          {/each}
        </div>
      {/if}
      {#if filtered.length === 0}
        <div class="flex min-h-52 flex-col items-center justify-center rounded-xl border border-neutral-200 bg-white px-6 text-center dark:border-white/[0.08] dark:bg-white/[0.05]">
          <Reicon name="search" class="mb-3 size-6 text-neutral-300 dark:text-fg-faint" />
          <p class="text-[13px] font-medium">No matching S3 storages</p>
          <p class="mt-1 text-[12px] text-neutral-500 dark:text-fg-dim">Try a different search.</p>
        </div>
      {/if}
    {/if}
  {/if}
</div>

{#if creating}
  <Modal title="New S3 Storage" onclose={() => (creating = false)}>
    <div class="w-full">
      <div class="mb-4">
        For more details, please visit the <a class="underline dark:text-brand-light" href="https://coolify.io/docs/knowledge-base/s3/introduction" target="_blank">Coolify Docs</a>.
      </div>
      <form class="flex flex-col gap-2" onsubmit={submit}>
        <div class="flex gap-2">
          <FormInput required label="Name" id="name" bind:value={name} />
          <FormInput label="Description" id="description" bind:value={description} />
        </div>
        <EndpointInput bind:parts />
        <div class="flex gap-2">
          <FormInput required label="Bucket" id="bucket" bind:value={bucket} />
          <FormInput required helper="Region only required for AWS. Leave it as-is for other providers." label="Region" id="region" bind:value={region} />
        </div>
        <div class="flex gap-2">
          <FormInput required type="password" label="Access Key" id="key" bind:value={key} />
          <FormInput required type="password" label="Secret Key" id="secret" bind:value={secret} />
        </div>
        <button class="button mt-4" type="submit" disabled={busy}>Validate Connection & Continue</button>
      </form>
    </div>
  </Modal>
{/if}
