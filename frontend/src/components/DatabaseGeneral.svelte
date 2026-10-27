<script lang="ts">
  // Port of upstream livewire/project/database/postgresql/general.blade.php
  // (+ Postgresql\General), with livewire/project/database/status-info.blade.php
  // (components/database-status-info.blade.php) as its Connection part and
  // livewire/project/shared/resource-details.blade.php behind "Details".
  // Making the database public, SSL, log drains, proxy logs and init script
  // edits have no backend in coolify-rs yet: they render as upstream renders
  // them and say so when used.
  import { untrack } from 'svelte';
  import Reicon from './Reicon.svelte';
  import Modal from './Modal.svelte';
  import Empty from './Empty.svelte';
  import Callout from './Callout.svelte';
  import Listbox from './Listbox.svelte';
  import FormInput from './FormInput.svelte';
  import FormTextarea from './FormTextarea.svelte';
  import UnsavedBar from './UnsavedBar.svelte';
  import SettingsSection from './SettingsSection.svelte';
  import DatabaseSubsection from './DatabaseSubsection.svelte';
  import { dbUi, ApiError, type DatabaseRecord } from '../lib/api';
  import { notify } from '../lib/toast.svelte';

  let { record, canUpdate, onsaved }: { record: DatabaseRecord; canUpdate: boolean; onsaved: () => Promise<void> } = $props();

  type Form = {
    name: string;
    description: string;
    image: string;
    postgresUser: string;
    postgresPassword: string;
    postgresDb: string;
    postgresInitdbArgs: string;
    postgresHostAuthMethod: string;
    postgresConf: string;
    publicPort: string;
    publicPortTimeout: string;
    portsMappings: string;
    customDockerRunOptions: string;
  };

  const fromRecord = (r: DatabaseRecord): Form => ({
    name: r.name ?? '',
    description: r.description ?? '',
    image: r.image ?? '',
    postgresUser: r.postgres_user ?? '',
    postgresPassword: r.postgres_password ?? '',
    postgresDb: r.postgres_db ?? '',
    postgresInitdbArgs: r.postgres_initdb_args ?? '',
    postgresHostAuthMethod: r.postgres_host_auth_method ?? '',
    postgresConf: r.postgres_conf ?? '',
    publicPort: r.public_port == null ? '' : String(r.public_port),
    publicPortTimeout: r.public_port_timeout == null ? '' : String(r.public_port_timeout),
    portsMappings: r.ports_mappings ?? '',
    customDockerRunOptions: r.custom_docker_run_options ?? '',
  });

  // The form starts from the record once; status refreshes of the record do not reset typing.
  let form = $state<Form>(untrack(() => fromRecord(record)));
  let saved = $state<Form>(untrack(() => fromRecord(record)));
  let saving = $state(false);
  let error = $state<ApiError | null>(null);
  let details = $state(false);

  // Listboxes whose changes coolify-rs cannot apply yet; they snap back to the record.
  let isPublic = $state(untrack(() => !!record.is_public));
  let enableSsl = $state(untrack(() => !!record.enable_ssl));
  let sslMode = $state(untrack(() => record.ssl_mode ?? 'require'));
  let logDrain = $state(untrack(() => !!record.is_log_drain_enabled));

  const hidden = $derived(!canUpdate);
  const started = $derived(!!record.started_at);
  const exited = $derived((record.status ?? '').startsWith('exited'));
  const dirty = $derived(JSON.stringify(form) !== JSON.stringify(saved));
  const err = (k: string) => error?.field(k);
  const urlHelper = 'If you change the user/password/port, this could be different. This is with the default values.';

  const sslModes = [
    { value: 'allow', label: 'allow (insecure)' },
    { value: 'prefer', label: 'prefer (secure)' },
    { value: 'require', label: 'require (secure)' },
    { value: 'verify-ca', label: 'verify-ca (secure)' },
    { value: 'verify-full', label: 'verify-full (secure)' },
  ];

  function unavailable(what: string) {
    notify.info('Not available in coolify-rs yet', `${what} is not supported by coolify-rs yet.`);
  }

  const orNull = (v: string) => (v.trim() === '' ? null : v);

  async function submit() {
    if (!canUpdate) return;
    saving = true;
    error = null;
    if (form.portsMappings) form.portsMappings = form.portsMappings.replaceAll(' ', '').trim();
    const body: Record<string, unknown> = {
      name: form.name,
      description: orNull(form.description),
      image: form.image,
      postgres_user: form.postgresUser,
      postgres_db: form.postgresDb,
      postgres_initdb_args: orNull(form.postgresInitdbArgs),
      postgres_host_auth_method: orNull(form.postgresHostAuthMethod),
      postgres_conf: orNull(form.postgresConf),
      ports_mappings: orNull(form.portsMappings),
      public_port: form.publicPort.trim() === '' ? null : Number(form.publicPort),
      public_port_timeout: form.publicPortTimeout.trim() === '' ? null : Number(form.publicPortTimeout),
      custom_docker_run_options: orNull(form.customDockerRunOptions),
      is_log_drain_enabled: !!record.is_log_drain_enabled,
    };
    if (record.postgres_password !== undefined) body.postgres_password = form.postgresPassword;
    try {
      await dbUi.update(record.uuid, body);
      notify.ok('Database updated.');
      saved = { ...form };
      await onsaved();
    } catch (e) {
      error = e instanceof ApiError ? e : new ApiError(0, String(e));
      if (!Object.keys(error.errors).length) notify.error(error.message);
    } finally {
      saving = false;
    }
  }
</script>

{#snippet copyInput(label: string, text: string)}
  <div class="w-full">
    <!-- svelte-ignore a11y_label_has_associated_control -->
    <label class="flex gap-1 items-center mb-1 text-sm font-medium text-black dark:text-white">{label}</label>
    <div class="relative">
      <input
        type="text"
        value={text}
        class="input input-with-copy-button bg-white dark:bg-coolgray-100 dark:read-only:bg-coolgray-100 dark:read-only:text-white"
        readonly
        onfocus={(e) => e.currentTarget.select()}
      />
      <button
        type="button"
        title="Copy to clipboard"
        aria-label="Copy to clipboard"
        class="icon-button group shrink-0 absolute top-1/2 right-2 -translate-y-1/2"
        onclick={() => navigator.clipboard?.writeText(text).catch(() => {})}
      >
        <span class="inline-flex transition-transform duration-150 ease-out group-active:scale-75"><Reicon name="copy" class="size-3.5" /></span>
      </button>
    </div>
  </div>
{/snippet}

<div class="application-settings-form flex flex-col gap-6">
  <form
    class="flex flex-col gap-6"
    onsubmit={(e) => {
      e.preventDefault();
      submit();
    }}
  >
    <UnsavedBar {dirty} {saving} onsave={submit} onreset={() => ((form = { ...saved }), (error = null))} />

    <SettingsSection id="database-details-section" title="Database details" description="Manage the identity, container image, and credentials of this PostgreSQL database.">
      {#snippet actions()}
        <button type="button" class="button" onclick={() => (details = true)}>Details</button>
      {/snippet}
      <div class="grid gap-4 lg:grid-cols-2">
        <FormInput label="Name" id="name" disabled={!canUpdate} bind:value={form.name} error={err('name')} />
        <FormInput label="Description" id="description" disabled={!canUpdate} bind:value={form.description} error={err('description')} />
        <div class="lg:col-span-2">
          <FormInput
            label="Image"
            id="image"
            required
            disabled={!canUpdate}
            helper="Use a published PostgreSQL image from Docker Hub."
            bind:value={form.image}
            error={err('image')}
          />
        </div>
      </div>
      <DatabaseSubsection id="credentials-section" title="Credentials" helper="Keep these values aligned with the credentials configured inside PostgreSQL.">
        {#if started}
          <Callout type="warning" title="Keep credentials synchronized">
            Changing values here does not update PostgreSQL. Update PostgreSQL first, then synchronize the values here so backups and other automations
            continue working.
          </Callout>
        {/if}
        <div class="{started ? 'mt-4 ' : ''}grid gap-4 lg:grid-cols-2">
          <FormInput
            label="Username"
            id="postgresUser"
            placeholder="If empty: postgres"
            disabled={!canUpdate}
            bind:value={form.postgresUser}
            error={err('postgres_user')}
          />
          {#if hidden}
            <FormInput label="Password" id="postgresPasswordHidden" disabled value="Hidden (only admins can view)" />
          {:else}
            <FormInput
              label="Password"
              id="postgresPassword"
              type="password"
              required
              bind:value={form.postgresPassword}
              error={err('postgres_password')}
            />
          {/if}
          <FormInput
            label="Initial database"
            id="postgresDb"
            placeholder="If empty, it will match the username."
            readonly={started}
            disabled={!canUpdate}
            helper={started ? 'You can only change this in the database.' : undefined}
            bind:value={form.postgresDb}
            error={err('postgres_db')}
          />
        </div>
      </DatabaseSubsection>
      <DatabaseSubsection
        id="initialization-section"
        title="Initialization"
        helper="Configure the options used when PostgreSQL creates its initial data directory."
      >
        <div class="grid gap-4 lg:grid-cols-2">
          <FormInput
            label="Initial database arguments"
            id="postgresInitdbArgs"
            placeholder="Leave empty to use the image default."
            disabled={!canUpdate}
            bind:value={form.postgresInitdbArgs}
            error={err('postgres_initdb_args')}
          />
          <FormInput
            label="Host authentication method"
            id="postgresHostAuthMethod"
            placeholder="Leave empty to use the image default."
            disabled={!canUpdate}
            bind:value={form.postgresHostAuthMethod}
            error={err('postgres_host_auth_method')}
          />
        </div>
      </DatabaseSubsection>
    </SettingsSection>

    <SettingsSection id="access-section" title="Access" description="How applications and clients connect to this database, and whether it is reachable from the internet.">
      {#snippet actions()}
        {#if isPublic}
          <button type="button" class="button" onclick={() => unavailable('Viewing the proxy logs')}>View logs</button>
        {/if}
      {/snippet}
      <DatabaseSubsection id="connection-section" title="Connection" helper="Use these URLs to connect to the database." first>
        <div class="space-y-5">
          {#if hidden}
            <FormInput label="Postgres URL (internal)" id="dbUrlHidden" disabled value="Hidden (only admins can view)" />
            <FormInput label="Postgres URL (public)" id="dbUrlPublicHidden" disabled value="Hidden (only admins can view)" />
          {:else}
            <FormInput label="Postgres URL (internal)" id="dbUrl" helper={urlHelper} type="password" readonly value={record.internal_db_url ?? ''} />
            {#if record.external_db_url}
              <FormInput label="Postgres URL (public)" id="dbUrlPublic" helper={urlHelper} type="password" readonly value={record.external_db_url} />
            {/if}
          {/if}

          <div class="border-t border-neutral-200 pt-5 dark:border-white/[0.06]">
            <div class="mb-4 flex items-center justify-between gap-3">
              <div>
                <h3 class="text-sm font-semibold text-black dark:text-fg">SSL configuration</h3>
                <p class="mt-1 text-xs text-neutral-500 dark:text-fg-dim">Encryption settings can only be changed while the database is stopped.</p>
              </div>
            </div>
            <div class="grid gap-4 sm:grid-cols-2">
              <Listbox
                id="enableSsl"
                label="SSL"
                disabled={!exited || !canUpdate}
                bind:value={enableSsl}
                onchange={() => {
                  enableSsl = !!record.enable_ssl;
                  unavailable('SSL for databases');
                }}
                options={[
                  { value: true, label: 'Enabled' },
                  { value: false, label: 'Disabled' },
                ]}
              />
              <Listbox
                id="sslMode"
                label="SSL mode"
                helper="Choose the SSL verification mode for PostgreSQL connections"
                disabled={!enableSsl || !exited || !canUpdate}
                bind:value={sslMode}
                onchange={() => {
                  sslMode = record.ssl_mode ?? 'require';
                  unavailable('SSL for databases');
                }}
                options={sslModes}
              />
            </div>
          </div>
        </div>
      </DatabaseSubsection>
      <DatabaseSubsection id="public-access-section" title="Public access" helper="Expose this database through the managed TCP proxy." class="relative">
        <div class="grid gap-4 lg:grid-cols-2">
          <Listbox
            id="isPublic"
            label="Access"
            disabled={!canUpdate}
            bind:value={isPublic}
            onchange={() => {
              isPublic = !!record.is_public;
              unavailable('Making a database public');
            }}
            options={[
              { value: false, label: 'Private' },
              {
                value: true,
                label: form.publicPort === '' ? 'Public through TCP proxy (set public port first)' : 'Public through TCP proxy',
                disabled: form.publicPort === '',
              },
            ]}
          />
          <FormInput
            type="number"
            placeholder="5432"
            disabled={isPublic || !canUpdate}
            id="publicPort"
            label="Public port"
            bind:value={form.publicPort}
            error={err('public_port')}
          />
          <FormInput
            type="number"
            placeholder="3600"
            disabled={isPublic || !canUpdate}
            id="publicPortTimeout"
            label="Proxy timeout"
            helper="Timeout in seconds. The default is 3600."
            bind:value={form.publicPortTimeout}
            error={err('public_port_timeout')}
          />
          <FormInput
            placeholder="3000:5432"
            id="portsMappings"
            label="Port mappings"
            helper="Comma-separated host-to-container mappings, for example 3000:5432."
            disabled={!canUpdate}
            bind:value={form.portsMappings}
            error={err('ports_mappings')}
          />
        </div>
      </DatabaseSubsection>
    </SettingsSection>

    <SettingsSection id="container-section" title="Container" description="Docker options, configuration, and log forwarding for the database container.">
      <FormInput
        helper="Add supported docker run options used when the container starts. Unsupported options can interfere with Coolify automation."
        placeholder="--cap-add SYS_ADMIN --device=/dev/fuse"
        id="customDockerRunOptions"
        label="Custom Docker options"
        disabled={!canUpdate}
        bind:value={form.customDockerRunOptions}
        error={err('custom_docker_run_options')}
      />
      <DatabaseSubsection id="configuration-section" title="Configuration" helper="Override the PostgreSQL configuration used by this container.">
        {#if hidden}
          <FormInput label="Custom PostgreSQL configuration" id="postgresConfHidden" disabled value="Hidden (only admins can view)" />
        {:else}
          <FormTextarea label="Custom PostgreSQL configuration" rows={10} id="postgresConf" bind:value={form.postgresConf} error={err('postgres_conf')} />
        {/if}
      </DatabaseSubsection>
      <DatabaseSubsection id="log-delivery-section" title="Log delivery" helper="Forward container logs to the drain configured on the server.">
        <Listbox
          id="isLogDrainEnabled"
          label="Log drain"
          disabled={!canUpdate}
          bind:value={logDrain}
          onchange={() => {
            logDrain = !!record.is_log_drain_enabled;
            unavailable('Forwarding logs to a log drain');
          }}
          options={[
            { value: false, label: 'Do not forward logs' },
            { value: true, label: 'Forward logs to the server drain' },
          ]}
        />
      </DatabaseSubsection>
    </SettingsSection>
  </form>

  <SettingsSection id="initialization-scripts-section" title="Initialization scripts" description="Run SQL files in order when PostgreSQL initializes for the first time." flush>
    {#snippet actions()}
      {#if canUpdate}
        <button type="button" class="button" onclick={() => unavailable('Adding initialization scripts')}>+ Add</button>
      {/if}
    {/snippet}
    <div class="divide-y divide-neutral-200 dark:divide-border-subtle">
      {#if hidden}
        <Empty title="Hidden (only admins can view)" description="Initialization scripts can contain credentials." />
      {:else if record.init_scripts?.length}
        {#each record.init_scripts as script (script.filename)}
          <!-- livewire/project/database/init-script.blade.php, read-only until coolify-rs can edit scripts -->
          <div class="application-settings-form flex flex-col gap-4 px-4 py-4">
            <div class="flex items-end gap-2">
              <FormInput id="init-script-{script.index}" label="Filename" value={script.filename} disabled />
              <button type="button" class="button button-error" disabled title="Not available in coolify-rs yet">Delete</button>
            </div>
            <FormTextarea id="init-script-content-{script.index}" label="Content" rows={12} value={script.content} disabled />
          </div>
        {/each}
      {:else}
        <Empty title="No initialization scripts" description="Add a SQL file to run during the first PostgreSQL initialization." />
      {/if}
    </div>
  </SettingsSection>
</div>

{#if details}
  <Modal title="Resource details" onclose={() => (details = false)}>
    <div class="w-full max-h-[70vh] overflow-y-auto pr-1 pt-1">
      <div class="flex flex-col gap-6">
        <div>
          <h3>Resource</h3>
          <div class="pt-2 grid grid-cols-1 gap-3 md:grid-cols-2">
            {@render copyInput('Name', record.name ?? '')}
            {@render copyInput('UUID', record.uuid ?? '')}
          </div>
        </div>
        {#if record.environment}
          <div>
            <h3>Environment</h3>
            <div class="pt-2 grid grid-cols-1 gap-3 md:grid-cols-2">
              {@render copyInput('Name', record.environment.name ?? '')}
              {@render copyInput('UUID', record.environment.uuid)}
            </div>
          </div>
          <div>
            <h3>Project</h3>
            <div class="pt-2 grid grid-cols-1 gap-3 md:grid-cols-2">
              {@render copyInput('Name', record.environment.project.name ?? '')}
              {@render copyInput('UUID', record.environment.project.uuid)}
            </div>
          </div>
        {/if}
        {#if record.destination?.server}
          <div>
            <h3>Server</h3>
            <div class="pt-2 grid grid-cols-1 gap-3 md:grid-cols-2">
              {@render copyInput('Name', record.destination.server.name ?? '')}
              {@render copyInput('UUID', record.destination.server.uuid)}
            </div>
          </div>
        {/if}
      </div>
    </div>
  </Modal>
{/if}
