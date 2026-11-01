<script lang="ts">
  // Port of upstream livewire/project/database/sqlite/general.blade.php (Project/Database/Sqlite/General)
  // with sqlite/connect-application.blade.php (Project/Database/Sqlite/ConnectApplication).
  import { untrack } from 'svelte';
  import Callout from './Callout.svelte';
  import CopyInput from './CopyInput.svelte';
  import DatabaseSubsection from './DatabaseSubsection.svelte';
  import FormInput from './FormInput.svelte';
  import Listbox from './Listbox.svelte';
  import Modal from './Modal.svelte';
  import Reicon from './Reicon.svelte';
  import SettingsSection from './SettingsSection.svelte';
  import StatusBadge from './StatusBadge.svelte';
  import UnsavedBar from './UnsavedBar.svelte';
  import { dbUi, ApiError, type DatabaseRecord } from '../lib/api';
  import { generalUi, type SqliteConnectState } from '../lib/databaseApi';
  import { link, router } from '../lib/router.svelte';
  import { notify } from '../lib/toast.svelte';

  let { record, canUpdate, onsaved }: { record: DatabaseRecord; canUpdate: boolean; onsaved: () => Promise<void> } = $props();

  const read = (r: DatabaseRecord) => ({
    name: r.name ?? '',
    description: r.description ?? '',
    image: r.image ?? '',
    sqlite_databases: String(r.sqlite_databases ?? ''),
    custom_docker_run_options: r.custom_docker_run_options ?? '',
  });
  let form = $state(untrack(() => read(record)));
  let saved = $state(untrack(() => read(record)));
  let saving = $state(false);
  let error = $state<ApiError | null>(null);
  let details = $state(false);
  let logDrain = $state(untrack(() => !!record.is_log_drain_enabled));
  const dirty = $derived(JSON.stringify(form) !== JSON.stringify(saved));
  const err = (k: string) => error?.field(k);

  const orNull = (v: string) => (v.trim() === '' ? null : v);
  const payload = () => ({
    name: form.name,
    description: orNull(form.description),
    image: form.image,
    sqlite_databases: form.sqlite_databases,
    custom_docker_run_options: orNull(form.custom_docker_run_options),
  });

  async function submit() {
    if (!canUpdate) return;
    saving = true;
    error = null;
    try {
      await dbUi.update(record.uuid, payload());
      notify.success('Database updated.');
      form.sqlite_databases = form.sqlite_databases
        .split(',')
        .map((n) => n.trim())
        .filter(Boolean)
        .join(',');
      saved = { ...form };
      await onsaved();
    } catch (e) {
      // `catch (Exception) { handleError }`: the message shows as a toast.
      const failure = e instanceof ApiError ? e : new ApiError(0, String(e));
      notify.failure(Object.values(failure.errors)[0]?.[0] ?? failure.message);
    } finally {
      saving = false;
    }
  }

  async function saveDrain(value: boolean) {
    const r = await generalUi.setLogDrain(record.uuid, value, payload());
    if (!r.ok) logDrain = !!record.is_log_drain_enabled;
    else await onsaved();
  }

  // ConnectApplication
  let connect = $state<SqliteConnectState | null>(null);
  let applicationUuid = $state('');
  let mountPath = $state('/var/lib/sqlite');
  let snippet = $state<string | null>(null);
  let snippetFor = $state<string | null>(null);
  let connectError = $state<ApiError | null>(null);

  const loadConnect = async () => (connect = await generalUi.sqliteState(record.uuid));
  $effect(() => {
    untrack(() => loadConnect());
  });

  async function mount() {
    snippet = null;
    snippetFor = null;
    connectError = null;
    try {
      const r = await generalUi.sqliteConnect(record.uuid, applicationUuid, mountPath);
      if (r.redirect) return router.go(r.redirect);
      if (r.compose_snippet) {
        snippet = r.compose_snippet;
        snippetFor = r.compose_application_name ?? '';
      }
    } catch (e) {
      connectError = e instanceof ApiError ? e : new ApiError(0, String(e));
      if (!Object.keys(connectError.errors).length) notify.failure(connectError.message);
    }
  }

  async function unlink(id: number) {
    await generalUi.sqliteUnlink(record.uuid, id);
    await loadConnect();
  }
</script>

<div class="application-settings-form">
  <form
    class="flex flex-col gap-6"
    onsubmit={(e) => {
      e.preventDefault();
      submit();
    }}
  >
    <UnsavedBar {dirty} {saving} onsave={submit} onreset={() => ((form = { ...saved }), (error = null))} />

    <SettingsSection id="database-details-section" title="Database details" description="Manage the identity, container image, and database files of this SQLite database.">
      {#snippet actions()}
        <StatusBadge label="Experimental" type="warning" />
        <button type="button" class="button" onclick={() => (details = true)}>Details</button>
      {/snippet}
      <div class="grid gap-4 lg:grid-cols-2">
        <FormInput label="Name" id="name" disabled={!canUpdate} bind:value={form.name} error={err('name')} />
        <FormInput label="Description" id="description" disabled={!canUpdate} bind:value={form.description} error={err('description')} />
        <div class="lg:col-span-2">
          <FormInput label="Image" id="image" required disabled={!canUpdate} helper="Use a published peakimages/sqlite image from Docker Hub." bind:value={form.image} error={err('image')} />
        </div>
      </div>
      <DatabaseSubsection id="database-files-section" title="Database files" helper="Applications use these databases by mounting the data volume.">
        <div class="grid gap-4 lg:grid-cols-2">
          <FormInput
            label="File names"
            id="sqliteDatabases"
            placeholder="database.sqlite"
            required
            disabled={!canUpdate}
            helper="Comma-separated file names. Missing files are created in the data volume on the next start. Removing a name does not delete its file. Backups restore into the first file."
            bind:value={form.sqlite_databases}
            error={err('sqlite_databases')}
          />
        </div>
      </DatabaseSubsection>
    </SettingsSection>

    <div>
      <SettingsSection
        title="Connect application"
        description="Mount the data volume of this database into an application on the same server. The application must run as user 65532 or as root to access the files."
      >
        {#if connect}
          {#if connect.connections.length || connect.compose_connections.length}
            <div class="mb-4 divide-y divide-neutral-200 rounded-lg border border-neutral-200 dark:divide-white/[0.08] dark:border-white/[0.08]">
              {#each connect.connections as c (c.id)}
                <div class="flex items-center justify-between gap-3 px-3 py-2 text-[13px]">
                  <span class="min-w-0 truncate">
                    <a href={c.href} use:link class="font-medium underline-offset-2 hover:underline">{c.application_name}</a>
                    <span class="text-neutral-500 dark:text-fg-dim">at {c.mount_path}</span>
                  </span>
                  <button type="button" class="button !px-2.5 !text-xs" disabled={!canUpdate} onclick={() => unlink(c.id)}>Unlink</button>
                </div>
              {/each}
              {#each connect.compose_connections as c (c.id)}
                <div class="flex items-center justify-between gap-3 px-3 py-2 text-[13px]">
                  <span class="min-w-0 truncate">
                    <a href={c.href} use:link class="font-medium underline-offset-2 hover:underline">{c.name}</a>
                    <span class="text-neutral-500 dark:text-fg-dim">in its compose file</span>
                  </span>
                  <span class="shrink-0 text-xs text-neutral-500 dark:text-fg-dim">Remove it from the compose file to unlink</span>
                </div>
              {/each}
            </div>
          {/if}
          {#if connect.application_options.length === 0}
            <p class="text-[13px] leading-5 text-neutral-500 dark:text-fg-dim">No applications are deployed on this server yet.</p>
          {:else}
            <div class="flex flex-col gap-4">
              <div class="grid gap-4 lg:grid-cols-2">
                <Listbox
                  searchable
                  id="applicationUuid"
                  label="Application"
                  required
                  options={connect.application_options}
                  placeholder="Select an application"
                  searchPlaceholder="Search applications…"
                  searchEmptyText="No matching applications"
                  helper="Docker Compose applications mount the volume from their compose file. coolify-rs shows the lines to add."
                  bind:value={applicationUuid}
                  error={connectError?.field('application_uuid')}
                />
                <FormInput
                  id="mountPath"
                  label="Mount path"
                  required
                  placeholder="/var/lib/sqlite"
                  disabled={!canUpdate}
                  helper="Any directory inside the application container. The database files ({(connect.sqlite_databases ?? '').replaceAll(',', ', ')}) will be available there."
                  bind:value={mountPath}
                  error={connectError?.field('mount_path')}
                  onkeydown={(e: KeyboardEvent) => {
                    if (e.key === 'Enter') {
                      e.preventDefault();
                      mount();
                    }
                  }}
                />
              </div>

              <div class="flex justify-end">
                <button type="button" class="button" disabled={!canUpdate} onclick={mount}>Mount volume</button>
              </div>

              {#if snippet}
                <Callout type="info" title="Add the volume to the compose file of {snippetFor}">
                  <p>Add these lines to the service that uses the database, then redeploy. Start this database first: Docker Compose does not create external volumes.</p>
                  <div class="relative mt-2">
                    <pre class="overflow-auto rounded-md bg-white p-3 pr-10 font-mono text-xs leading-5 text-neutral-700 ring-1 ring-neutral-200 dark:bg-black/20 dark:text-neutral-300 dark:ring-white/[0.08]">{snippet}</pre>
                    <button
                      type="button"
                      class="icon-button absolute top-2 right-2"
                      title="Copy compose lines"
                      aria-label="Copy compose lines"
                      onclick={() => navigator.clipboard?.writeText(snippet ?? '').catch(() => {})}
                    >
                      <Reicon name="copy" class="size-3.5" />
                    </button>
                  </div>
                </Callout>
              {/if}
            </div>
          {/if}
        {/if}
      </SettingsSection>
    </div>

    <SettingsSection id="container-section" title="Container" description="Docker options and log forwarding for the database container.">
      <FormInput
        helper="Add supported docker run options used when the container starts. Unsupported options can interfere with coolify-rs automation."
        placeholder="--cap-add SYS_ADMIN --device=/dev/fuse"
        id="customDockerRunOptions"
        label="Custom Docker options"
        disabled={!canUpdate}
        bind:value={form.custom_docker_run_options}
        error={err('custom_docker_run_options')}
      />
      <DatabaseSubsection id="log-delivery-section" title="Log delivery" helper="Forward container logs to the drain configured on the server.">
        <Listbox
          id="isLogDrainEnabled"
          label="Log drain"
          disabled={!canUpdate}
          bind:value={logDrain}
          onchange={(v: boolean) => saveDrain(v)}
          options={[
            { value: false, label: 'Do not forward logs' },
            { value: true, label: 'Forward logs to the server drain' },
          ]}
        />
      </DatabaseSubsection>
    </SettingsSection>
  </form>
</div>

{#if details}
  <Modal title="Resource details" onclose={() => (details = false)}>
    <div class="w-full max-h-[70vh] overflow-y-auto pr-1 pt-1">
      <div class="flex flex-col gap-6">
        <div>
          <h3>Resource</h3>
          <div class="pt-2 grid grid-cols-1 gap-3 md:grid-cols-2">
            <CopyInput label="Name" text={record.name ?? ''} />
            <CopyInput label="UUID" text={record.uuid ?? ''} />
          </div>
        </div>
        {#if record.environment}
          <div>
            <h3>Environment</h3>
            <div class="pt-2 grid grid-cols-1 gap-3 md:grid-cols-2">
              <CopyInput label="Name" text={record.environment.name ?? ''} />
              <CopyInput label="UUID" text={record.environment.uuid} />
            </div>
          </div>
          <div>
            <h3>Project</h3>
            <div class="pt-2 grid grid-cols-1 gap-3 md:grid-cols-2">
              <CopyInput label="Name" text={record.environment.project.name ?? ''} />
              <CopyInput label="UUID" text={record.environment.project.uuid} />
            </div>
          </div>
        {/if}
        {#if record.destination?.server}
          <div>
            <h3>Server</h3>
            <div class="pt-2 grid grid-cols-1 gap-3 md:grid-cols-2">
              <CopyInput label="Name" text={record.destination.server.name ?? ''} />
              <CopyInput label="UUID" text={record.destination.server.uuid} />
            </div>
          </div>
        {/if}
      </div>
    </div>
  </Modal>
{/if}
