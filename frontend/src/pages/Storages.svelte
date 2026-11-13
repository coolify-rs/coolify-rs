<script lang="ts">
  // Port of upstream livewire/storage/index.blade.php and livewire/storage/create.blade.php.
  import Reicon from '../components/Reicon.svelte';
  import Empty from '../components/Empty.svelte';
  import Modal from '../components/Modal.svelte';
  import ViewToggle from '../components/ViewToggle.svelte';
  import StatusBadge from '../components/StatusBadge.svelte';
  import FormInput from '../components/FormInput.svelte';
  import Checkbox from '../components/Checkbox.svelte';
  import EndpointInput from '../components/EndpointInput.svelte';
  import Listbox from '../components/Listbox.svelte';
  import StorageServerFields from '../components/StorageServerFields.svelte';
  import StorageSftpLogin from '../components/StorageSftpLogin.svelte';
  import { ApiError } from '../lib/api';
  import { storagesUi, composeEndpoint, detectPathStyleEndpoint, type EndpointParts, type PrivateKeyOption, type ServerParts, type StorageRow, type StorageType } from '../lib/storagesApi';
  import { link, router } from '../lib/router.svelte';
  import { notify } from '../lib/toast.svelte';

  document.title = 'Storages | coolify-rs';

  let storages = $state<StorageRow[] | null>(null);
  let canCreate = $state(false);
  let types = $state<{ value: StorageType; label: string }[]>([]);
  let privateKeys = $state<PrivateKeyOption[]>([]);
  storagesUi.list().then((d) => {
    storages = d.storages;
    canCreate = d.can_create;
    types = d.types;
    privateKeys = d.private_keys;
  });

  const describe = (s: StorageRow) => s.description || (s.type === 's3' ? 'S3-compatible storage' : `${s.type_label} storage`);
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
  // GlobalSearch's `open-create-modal-storage` arrives as `?create=1`.
  let creating = $state(router.query('create') === '1');
  let type = $state<StorageType>('s3');
  let name = $state('');
  let description = $state('');
  let parts = $state<EndpointParts>({ scheme: 'https', host: '', port: '', path: '' });
  let bucket = $state('');
  let region = $state('us-east-1');
  let key = $state('');
  let secret = $state('');
  let usePathStyleEndpoint = $state(true);
  let usePathStyleEndpointChanged = $state(false);
  let skipTlsVerification = $state(false);
  let serverParts = $state<ServerParts>({ host: '', port: '', path: '' });
  let sftpAuth = $state('password');
  let privateKeyId = $state<number | null>(null);
  let busy = $state(false);

  /** `updatedEndpointParts`: the URL style follows the provider until the user picks one. */
  function endpointChanged() {
    if (!usePathStyleEndpointChanged) usePathStyleEndpoint = detectPathStyleEndpoint(composeEndpoint(parts));
  }

  async function submit(e: SubmitEvent) {
    e.preventDefault();
    busy = true;
    try {
      const r = await storagesUi.create({
        type,
        name,
        description,
        endpoint: composeEndpoint(parts),
        server_parts: serverParts,
        bucket,
        region,
        key,
        secret,
        use_path_style_endpoint: usePathStyleEndpoint,
        skip_tls_verification: skipTlsVerification,
        sftp_auth: sftpAuth,
        private_key_id: privateKeyId,
      });
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
      <h1 class="truncate text-[24px]! leading-7! font-semibold! tracking-tight!">Storage</h1>
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
      <Empty title="No storage yet" description="Add an S3-compatible, SFTP, FTPS, or WebDAV destination to store backups outside your servers." iconName="storages" />
    {:else}
      <div class="mb-4 flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
        <div class="w-full sm:max-w-sm">
          <div class="relative w-full">
            <Reicon name="search" class="pointer-events-none absolute top-1/2 left-2.5 z-10 size-3.5 -translate-y-1/2 text-neutral-400 dark:text-fg-faint" />
            <input
              bind:value={search}
              type="search"
              placeholder="Search storages"
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
  <Modal title="New Storage" onclose={() => (creating = false)}>
    <div class="w-full">
      <div class="mb-4">
        For more details, please visit the <a class="underline dark:text-brand-light" href="https://coolify.io/docs/knowledge-base/s3/introduction" target="_blank">Coolify Docs</a>.
      </div>
      <form class="flex flex-col gap-2" onsubmit={submit}>
        <div class="w-64">
          <Listbox required id="type" label="Type" options={types} bind:value={type} />
        </div>
        <div class="flex gap-2">
          <FormInput required label="Name" id="name" bind:value={name} />
          <FormInput label="Description" id="description" bind:value={description} />
        </div>
        {#if type === 's3'}
          <!-- Sends the endpoint right away, so the URL style below follows the provider. -->
          <div onchange={endpointChanged}>
            <EndpointInput bind:parts />
          </div>
          <div class="flex gap-2">
            <FormInput required label="Bucket" id="bucket" bind:value={bucket} />
            <FormInput required helper="Region only required for AWS. Leave it as-is for other providers." label="Region" id="region" bind:value={region} />
          </div>
          <div class="flex gap-2">
            <FormInput required type="password" label="Access Key" id="key" bind:value={key} />
            <FormInput required type="password" label="Secret Key" id="secret" bind:value={secret} />
          </div>
          <div class="w-64">
            <Checkbox id="usePathStyleEndpoint" label="Use path-style URLs" helper="Turn this off for providers that only accept virtual-hosted-style URLs (bucket.endpoint). coolify-rs turns it off for Alibaba Cloud OSS, Tencent Cloud COS, and Volcengine TOS, and when the provider rejects path-style URLs." bind:checked={usePathStyleEndpoint} onchange={() => (usePathStyleEndpointChanged = true)} />
            <Checkbox id="skipTlsVerification" label="Skip TLS certificate verification" helper="Turn this on only for an endpoint with a self-signed or private CA certificate. coolify-rs and the backup helper then accept any certificate, so an attacker on the network path can read or change your backups. Browsers that download files from this storage must still trust the certificate." bind:checked={skipTlsVerification} />
          </div>
        {:else}
          {#if type === 'webdav'}
            <EndpointInput bind:parts hostPlaceholder="cloud.example.com" portPlaceholder="443" pathPlaceholder="/remote.php/dav/files/USER/backups" pathHelper="The path of the backup directory on the WebDAV server, for example /remote.php/dav/files/USER/backups for Nextcloud." />
          {:else}
            <StorageServerFields bind:parts={serverParts} defaultPort={type === 'sftp' ? 22 : 21} />
          {/if}
          {#if type === 'sftp'}
            <FormInput required label="Username" id="key" autocomplete="off" bind:value={key} />
            <StorageSftpLogin bind:auth={sftpAuth} bind:privateKeyId bind:secret {privateKeys} />
          {:else}
            <div class="flex gap-2">
              <FormInput required label="Username" id="key" autocomplete="off" bind:value={key} />
              <FormInput required type="password" label="Password" id="secret" bind:value={secret} />
            </div>
          {/if}
          <p class="text-xs text-neutral-500 dark:text-fg-dim">
            Backups keep their full path on the server (for example data/coolify/backups/...) below this directory. coolify-rs connects from its own container to check, restore, download, and delete backups, and from each server to upload them.
          </p>
        {/if}

        <button class="button mt-4" type="submit" disabled={busy}>Validate Connection & Continue</button>
      </form>
    </div>
  </Modal>
{/if}
