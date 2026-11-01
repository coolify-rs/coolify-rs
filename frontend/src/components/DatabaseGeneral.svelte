<script lang="ts">
  // Port of upstream livewire/project/database/<type>/general.blade.php: Project/Database/Postgresql/General,
  // Project/Database/Redis/General, Project/Database/Keydb/General, Project/Database/Dragonfly/General,
  // Project/Database/Mongodb/General, Project/Database/Mysql/General, Project/Database/Mariadb/General and
  // Project/Database/Clickhouse/General, with livewire/project/database/status-info.blade.php
  // (components/database-status-info.blade.php; Project/Database/Postgresql/StatusInfo,
  // Project/Database/Redis/StatusInfo, Project/Database/Keydb/StatusInfo, Project/Database/Dragonfly/StatusInfo,
  // Project/Database/Mongodb/StatusInfo, Project/Database/Mysql/StatusInfo, Project/Database/Mariadb/StatusInfo and
  // Project/Database/Clickhouse/StatusInfo) as its Connection part and
  // livewire/project/shared/resource-details.blade.php behind "Details". compat/notes/database-general-forms.md
  // lists how the types differ.
  import { untrack } from 'svelte';
  import Reicon from './Reicon.svelte';
  import Modal from './Modal.svelte';
  import Empty from './Empty.svelte';
  import Callout from './Callout.svelte';
  import ConfirmModal from './ConfirmModal.svelte';
  import LogViewer, { type LogRequest } from './LogViewer.svelte';
  import ProcessDialog from './ProcessDialog.svelte';
  import Listbox from './Listbox.svelte';
  import FormInput from './FormInput.svelte';
  import FormTextarea from './FormTextarea.svelte';
  import UnsavedBar from './UnsavedBar.svelte';
  import SettingsSection from './SettingsSection.svelte';
  import DatabaseSubsection from './DatabaseSubsection.svelte';
  import { dbUi, ApiError, type DatabaseRecord, type DatabaseFormExtras } from '../lib/api';
  import { generalUi } from '../lib/databaseApi';
  import { notify } from '../lib/toast.svelte';

  let {
    record,
    kind,
    extras,
    canUpdate,
    onsaved,
  }: { record: DatabaseRecord; kind: string; extras: DatabaseFormExtras; canUpdate: boolean; onsaved: () => Promise<void> } = $props();

  // Per-type strings and options of the views.
  const names: Record<string, string> = {
    postgresql: 'PostgreSQL',
    redis: 'Redis',
    keydb: 'KeyDB',
    dragonfly: 'Dragonfly',
    mongodb: 'MongoDB',
    mysql: 'MySQL',
    mariadb: 'MariaDB',
    clickhouse: 'ClickHouse',
  };
  const imageHelpers: Record<string, string | undefined> = {
    postgresql: 'Use a published PostgreSQL image from Docker Hub.',
    redis: 'Use a published Redis image from Docker Hub.',
    keydb: 'Use a published eqalpha/keydb image from Docker Hub.',
    dragonfly: undefined,
    mongodb: 'Use a published MongoDB image from Docker Hub.',
    mysql: 'Use a published MySQL image from Docker Hub.',
    mariadb: 'Use a published MariaDB image from Docker Hub.',
    clickhouse: 'Use a published clickhouse/clickhouse-server image from Docker Hub.',
  };
  const ports: Record<string, string> = {
    postgresql: '5432',
    redis: '6379',
    keydb: '6379',
    dragonfly: '6379',
    mongodb: '27017',
    mysql: '3306',
    mariadb: '3306',
    clickhouse: '8123',
  };
  type Conf = { column: string; label: string; section: string; helper?: string; placeholder?: string };
  const confs: Record<string, Conf | null> = {
    postgresql: { column: 'postgres_conf', label: 'Custom PostgreSQL configuration', section: 'Override the PostgreSQL configuration used by this container.' },
    redis: {
      column: 'redis_conf',
      label: 'Custom Redis configuration',
      section: 'Override only the Redis directives you need. All other defaults remain active.',
      helper: 'coolify-rs automatically applies requirepass using the password above. If you override requirepass here, keep both values identical.',
      placeholder: '# maxmemory 256mb\n    # maxmemory-policy allkeys-lru\n    # timeout 300',
    },
    keydb: {
      column: 'keydb_conf',
      label: 'Custom KeyDB configuration',
      section: 'Override only the KeyDB directives you need. All other defaults remain active.',
      helper: "<a target='_blank' class='underline' href='https://raw.githubusercontent.com/Snapchat/KeyDB/unstable/keydb.conf'>View the default KeyDB configuration</a>",
    },
    dragonfly: null,
    mongodb: { column: 'mongo_conf', label: 'Custom MongoDB configuration', section: 'Override the MongoDB configuration used by this container.' },
    mysql: { column: 'mysql_conf', label: 'Custom MySQL configuration', section: 'Override the MySQL configuration used by this container.' },
    mariadb: { column: 'mariadb_conf', label: 'Custom MariaDB configuration', section: 'Override the MariaDB configuration used by this container.' },
    clickhouse: null,
  };
  // The status-info label (`databaseLabel()`).
  const urlLabels: Record<string, string> = {
    postgresql: 'Postgres',
    redis: 'Redis',
    keydb: 'KeyDB',
    dragonfly: 'Dragonfly',
    mongodb: 'Mongo',
    mysql: 'MySQL',
    mariadb: 'MariaDB',
    clickhouse: 'Clickhouse',
  };
  type SslModes = { options: { value: string; label: string }[]; helper: string; fallback: string } | null;
  const sslModeOptions: Record<string, SslModes> = {
    postgresql: {
      options: [
        { value: 'allow', label: 'allow (insecure)' },
        { value: 'prefer', label: 'prefer (secure)' },
        { value: 'require', label: 'require (secure)' },
        { value: 'verify-ca', label: 'verify-ca (secure)' },
        { value: 'verify-full', label: 'verify-full (secure)' },
      ],
      helper: 'Choose the SSL verification mode for PostgreSQL connections',
      fallback: 'require',
    },
    mongodb: {
      options: [
        { value: 'allow', label: 'allow (insecure)' },
        { value: 'prefer', label: 'prefer (secure)' },
        { value: 'require', label: 'require (secure)' },
        { value: 'verify-full', label: 'verify-full (secure)' },
      ],
      helper: 'Choose the SSL verification mode for MongoDB connections',
      fallback: 'require',
    },
    mysql: {
      options: [
        { value: 'PREFERRED', label: 'Prefer (secure)' },
        { value: 'REQUIRED', label: 'Require (secure)' },
        { value: 'VERIFY_CA', label: 'Verify CA (secure)' },
        { value: 'VERIFY_IDENTITY', label: 'Verify Full (secure)' },
      ],
      helper: 'Choose the SSL verification mode for MySQL connections',
      fallback: 'REQUIRED',
    },
  };
  const supportsSsl = $derived(kind !== 'clickhouse');
  const sslModes = $derived(sslModeOptions[kind] ?? null);
  const publicPlaceholder = $derived(['keydb', 'dragonfly', 'clickhouse'].includes(kind));
  const lockHidesUrl = $derived(['redis', 'keydb', 'dragonfly'].includes(kind) && extras.credential_locked);
  const conf = $derived(confs[kind] ?? null);
  const label = $derived(names[kind] ?? kind);
  const port = $derived(ports[kind] ?? '');

  // The credential fields of each type (snake_case columns, as the endpoint takes them).
  const credentialColumns: Record<string, string[]> = {
    postgresql: ['postgres_user', 'postgres_password', 'postgres_db', 'postgres_initdb_args', 'postgres_host_auth_method'],
    redis: ['redis_username', 'redis_password'],
    keydb: ['keydb_password'],
    dragonfly: ['dragonfly_password'],
    mongodb: ['mongo_initdb_root_username', 'mongo_initdb_root_password', 'mongo_initdb_database'],
    mysql: ['mysql_root_password', 'mysql_user', 'mysql_password', 'mysql_database'],
    mariadb: ['mariadb_root_password', 'mariadb_user', 'mariadb_password', 'mariadb_database'],
    clickhouse: ['clickhouse_admin_user', 'clickhouse_admin_password'],
  };

  type Form = {
    name: string;
    description: string;
    image: string;
    conf: string;
    creds: Record<string, string>;
    publicPort: string;
    publicPortTimeout: string;
    portsMappings: string;
    customDockerRunOptions: string;
  };

  const str = (v: unknown) => (v == null ? '' : String(v));
  const fromRecord = (r: DatabaseRecord): Form => {
    const creds: Record<string, string> = {};
    for (const c of credentialColumns[kind] ?? []) creds[c] = str(r[c]);
    if (kind === 'redis') {
      creds.redis_username = extras.redis_username ?? 'default';
      creds.redis_password = str(extras.redis_password);
    }
    return {
      name: r.name ?? '',
      description: r.description ?? '',
      image: r.image ?? '',
      conf: conf ? str(r[conf.column]) : '',
      creds,
      publicPort: r.public_port == null ? '' : String(r.public_port),
      publicPortTimeout: r.public_port_timeout == null ? '' : String(r.public_port_timeout),
      portsMappings: r.ports_mappings ?? '',
      customDockerRunOptions: r.custom_docker_run_options ?? '',
    };
  };

  // The form starts from the record once; status refreshes of the record do not reset typing.
  let form = $state<Form>(untrack(() => fromRecord(record)));
  let saved = $state<Form>(untrack(() => fromRecord(record)));
  let saving = $state(false);
  let error = $state<ApiError | null>(null);
  let details = $state(false);

  let confirmRegenerate = $state(false);
  let confirmKeep = $state(false);
  let proxyLogs = $state(false);
  let anonymousVolume = $state<string | null>(null);
  let addingScript = $state(false);
  let newFilename = $state('');
  let newContent = $state('');
  let scriptError = $state<ApiError | null>(null);
  let scripts = $state<{ index: number; filename: string; content: string; original: string }[]>(
    untrack(() => (record.init_scripts ?? []).map((s) => ({ ...s, original: s.filename }))),
  );
  let deletingScript = $state<string | null>(null);

  let isPublic = $state(untrack(() => !!record.is_public));
  let enableSsl = $state(untrack(() => !!record.enable_ssl));
  let sslMode = $state(untrack(() => str(record.ssl_mode) || (sslModeOptions[kind]?.fallback ?? '')));
  let logDrain = $state(untrack(() => !!record.is_log_drain_enabled));

  const hidden = $derived(!canUpdate);
  const started = $derived(!!record.started_at);
  const exited = $derived((record.status ?? '').includes('exited'));
  const dirty = $derived(JSON.stringify(form) !== JSON.stringify(saved));
  const err = (k: string) => error?.field(k);
  const urlHelper = 'If you change the user/password/port, this could be different. This is with the default values.';

  const orNull = (v: string) => (v.trim() === '' ? null : v);
  // Hidden or locked passwords never reach the form, so they are not sent back.
  const sendable = (column: string) => {
    if (kind === 'redis') return column !== 'redis_password' || (!extras.credential_locked && extras.redis_password !== undefined);
    return !column.endsWith('password') || record[column] !== undefined;
  };

  function formBody(): Record<string, unknown> {
    if (form.portsMappings) form.portsMappings = form.portsMappings.replaceAll(' ', '').trim();
    const body: Record<string, unknown> = {
      name: form.name,
      description: orNull(form.description),
      image: form.image,
      ports_mappings: orNull(form.portsMappings),
      public_port: str(form.publicPort).trim() === '' ? null : Number(form.publicPort),
      public_port_timeout: str(form.publicPortTimeout).trim() === '' ? null : Number(form.publicPortTimeout),
      custom_docker_run_options: orNull(form.customDockerRunOptions),
      is_log_drain_enabled: !!record.is_log_drain_enabled,
    };
    if (conf) body[conf.column] = orNull(form.conf);
    for (const [column, value] of Object.entries(form.creds)) {
      if (!sendable(column)) continue;
      body[column] = ['postgres_initdb_args', 'postgres_host_auth_method'].includes(column) ? orNull(value) : value;
    }
    return body;
  }

  async function submit() {
    if (!canUpdate) return;
    saving = true;
    error = null;
    const body = formBody();
    try {
      await dbUi.update(record.uuid, body);
      notify.success('Database updated.');
      saved = JSON.parse(JSON.stringify(form));
      await onsaved();
    } catch (e) {
      failed(e);
    } finally {
      saving = false;
    }
  }

  function failed(e: unknown) {
    // `catch (Exception) { handleError }`: the message shows as a toast, not next to the field.
    const failure = e instanceof ApiError ? e : new ApiError(0, String(e));
    notify.failure(Object.values(failure.errors)[0]?.[0] ?? failure.message);
  }

  // `General::instantSave`
  async function savePublic(value: boolean) {
    try {
      const r = await generalUi.setPublic(record.uuid, value, formBody());
      if (!r.ok) isPublic = !!record.is_public;
      else saved = JSON.parse(JSON.stringify(form));
      await onsaved();
    } catch (e) {
      isPublic = !!record.is_public;
      failed(e);
    }
  }

  // `General::instantSaveAdvanced`
  async function saveLogDrain(value: boolean) {
    try {
      const r = await generalUi.setLogDrain(record.uuid, value, formBody());
      if (!r.ok) logDrain = !!record.is_log_drain_enabled;
      else saved = JSON.parse(JSON.stringify(form));
      await onsaved();
    } catch (e) {
      logDrain = !!record.is_log_drain_enabled;
      failed(e);
    }
  }

  // `HasDatabaseStatusInfo::instantSaveSSL`
  async function saveSsl() {
    await generalUi.setSsl(record.uuid, enableSsl, sslModes ? sslMode : null);
    await onsaved();
  }

  // `Clickhouse\General::mount`
  $effect(() => {
    if (kind === 'clickhouse') {
      generalUi
        .anonymousVolume(record.uuid)
        .then((r) => (anonymousVolume = r.volume))
        .catch(() => {});
    }
  });

  async function fetchProxyLogs(req: LogRequest) {
    return (await generalUi.proxyLogs(record.uuid, req.lines, req.timestamps)).logs;
  }

  // `save_new_init_script`
  async function addScript(e: SubmitEvent) {
    e.preventDefault();
    scriptError = null;
    try {
      const r = await generalUi.initScript(record.uuid, { action: 'add', filename: newFilename, content: newContent });
      if (r.ok) {
        scripts = [...scripts, { index: scripts.length, filename: newFilename, content: newContent, original: newFilename }];
        newFilename = '';
        newContent = '';
        addingScript = false;
        await onsaved();
      }
    } catch (err) {
      scriptError = err instanceof ApiError ? err : new ApiError(0, String(err));
    }
  }

  // `InitScript::submit` and `save_init_script`
  async function saveScript(script: (typeof scripts)[number]) {
    const r = await generalUi.initScript(record.uuid, { action: 'save', filename: script.filename, content: script.content, original_filename: script.original });
    if (r.ok) {
      script.original = script.filename;
      await onsaved();
    }
  }

  // `InitScript::delete` and `delete_init_script`
  async function deleteScript(filename: string) {
    const r = await generalUi.initScript(record.uuid, { action: 'delete', filename });
    if (r.ok) {
      scripts = scripts.filter((s) => s.original !== filename).map((s, i) => ({ ...s, index: i }));
      await onsaved();
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

{#snippet hiddenInput(text: string, id: string)}
  <FormInput label={text} {id} disabled value="Hidden (only admins can view)" />
{/snippet}

{#snippet credentials()}
  {#if kind === 'postgresql'}
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
          bind:value={form.creds.postgres_user}
          error={err('postgres_user')}
        />
        {#if hidden}
          {@render hiddenInput('Password', 'postgresPasswordHidden')}
        {:else}
          <FormInput
            label="Password"
            id="postgresPassword"
            type="password"
            required
            bind:value={form.creds.postgres_password}
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
          bind:value={form.creds.postgres_db}
          error={err('postgres_db')}
        />
      </div>
    </DatabaseSubsection>
    <DatabaseSubsection id="initialization-section" title="Initialization" helper="Configure the options used when PostgreSQL creates its initial data directory.">
      <div class="grid gap-4 lg:grid-cols-2">
        <FormInput
          label="Initial database arguments"
          id="postgresInitdbArgs"
          placeholder="Leave empty to use the image default."
          disabled={!canUpdate}
          bind:value={form.creds.postgres_initdb_args}
          error={err('postgres_initdb_args')}
        />
        <FormInput
          label="Host authentication method"
          id="postgresHostAuthMethod"
          placeholder="Leave empty to use the image default."
          disabled={!canUpdate}
          bind:value={form.creds.postgres_host_auth_method}
          error={err('postgres_host_auth_method')}
        />
      </div>
    </DatabaseSubsection>
  {:else if kind === 'redis'}
    <DatabaseSubsection id="credentials-section" title="Credentials" helper="Keep these values aligned with the credentials configured inside Redis.">
      <Callout type="warning" title={started ? 'Keep credentials synchronized' : 'Verify the initial credentials'}>
        {#if started}
          Changing values here does not update Redis. Update Redis first, then synchronize the values here so automations continue working.
        {:else}
          These values can only be changed here before the first start.
        {/if}
      </Callout>
      <div class="mt-4 grid gap-4 lg:grid-cols-2">
        {#if extras.redis_version_6}
          <FormInput
            label="Username"
            id="redisUsername"
            required={!started}
            helper={started ? 'You can only change this in the database.' : 'Shared REDIS_USERNAME values make this field read-only.'}
            disabled={!canUpdate || (!started && !!extras.shared_redis_username)}
            bind:value={form.creds.redis_username}
            error={err('redis_username')}
          />
        {/if}
        {#if hidden}
          {@render hiddenInput('Password', 'redisPasswordHidden')}
        {:else if extras.credential_locked}
          <FormInput label="Password" id="redisPasswordLocked" disabled value="Locked (shown once)" helper="REDIS_PASSWORD is locked. Its value is never shown again." />
        {:else}
          <FormInput
            label="Password"
            id="redisPassword"
            type="password"
            required={!started}
            helper={started ? 'You can only change this in the database.' : 'Shared REDIS_PASSWORD values make this field read-only.'}
            disabled={!started && !!extras.shared_redis_password}
            bind:value={form.creds.redis_password}
            error={err('redis_password')}
          />
        {/if}
      </div>
    </DatabaseSubsection>
  {:else if kind === 'keydb' || kind === 'dragonfly'}
    {@const column = `${kind}_password`}
    <DatabaseSubsection
      id="credentials-section"
      title="Credentials"
      helper={kind === 'keydb'
        ? 'Keep this value aligned with the password configured inside KeyDB.'
        : 'Keep these values aligned with the credentials configured inside Dragonfly.'}
    >
      {#if started}
        {#if hidden}
          {@render hiddenInput('Password', `${kind}PasswordHidden`)}
        {:else}
          <FormInput
            label="Password"
            id="{kind}Password"
            type="password"
            required
            readonly
            helper="You can only change this in the database."
            bind:value={form.creds[column]}
            error={err(column)}
          />
        {/if}
      {:else}
        <Callout type="warning" title="Verify the initial credentials">
          You can only change this password here before the first start. Later changes must be made inside the database.
        </Callout>
        <div class="mt-4">
          {#if hidden}
            {@render hiddenInput('Password', `${kind}PasswordHidden`)}
          {:else}
            <FormInput label="Password" id="{kind}Password" type="password" required bind:value={form.creds[column]} error={err(column)} />
          {/if}
        </div>
      {/if}
    </DatabaseSubsection>
  {:else if kind === 'mongodb'}
    <DatabaseSubsection id="credentials-section" title="Credentials" helper="Keep these values aligned with the credentials configured inside MongoDB.">
      {#if started}
        <Callout type="warning" title="Keep credentials synchronized">
          Changing values here does not update MongoDB. Update MongoDB first, then synchronize the values here so backups and other automations continue
          working.
        </Callout>
      {/if}
      <div class="{started ? 'mt-4 ' : ''}grid gap-4 lg:grid-cols-2">
        <FormInput
          label={started ? 'Initial username' : 'Username'}
          id="mongoInitdbRootUsername"
          placeholder="If empty: root"
          required={!started}
          disabled={!canUpdate}
          bind:value={form.creds.mongo_initdb_root_username}
          error={err('mongo_initdb_root_username')}
        />
        {#if hidden}
          {@render hiddenInput('Password', 'mongoInitdbRootPasswordHidden')}
        {:else}
          <FormInput
            label={started ? 'Initial password' : 'Password'}
            id="mongoInitdbRootPassword"
            type="password"
            required
            bind:value={form.creds.mongo_initdb_root_password}
            error={err('mongo_initdb_root_password')}
          />
        {/if}
        <FormInput
          label={started ? 'Initial database' : 'Database'}
          id="mongoInitdbDatabase"
          placeholder="If empty, it will match the username."
          readonly={started}
          required={!started}
          disabled={!canUpdate}
          helper={started ? 'You can only change this in the database.' : undefined}
          bind:value={form.creds.mongo_initdb_database}
          error={err('mongo_initdb_database')}
        />
      </div>
    </DatabaseSubsection>
  {:else if kind === 'mysql' || kind === 'mariadb'}
    {@const p = kind}
    <DatabaseSubsection id="credentials-section" title="Credentials" helper="Keep these values aligned with the credentials configured inside {label}.">
      {#if started}
        <Callout type="warning" title="Keep credentials synchronized">
          Changing values here does not update {label}. Update {label} first, then synchronize the values here so backups and other automations continue
          working.
        </Callout>
      {/if}
      <div class="{started ? 'mt-4 ' : ''}grid gap-4 lg:grid-cols-2">
        {#if hidden}
          {@render hiddenInput('Root password', `${p}RootPasswordHidden`)}
        {:else}
          <FormInput
            label="Root password"
            id="{p}RootPassword"
            type="password"
            required={started}
            bind:value={form.creds[`${p}_root_password`]}
            error={err(`${p}_root_password`)}
          />
        {/if}
        <FormInput label="Normal user" id="{p}User" required disabled={!canUpdate} bind:value={form.creds[`${p}_user`]} error={err(`${p}_user`)} />
        {#if hidden}
          {@render hiddenInput('Normal user password', `${p}PasswordHidden`)}
        {:else}
          <FormInput
            label="Normal user password"
            id="{p}Password"
            type="password"
            required
            bind:value={form.creds[`${p}_password`]}
            error={err(`${p}_password`)}
          />
        {/if}
        <FormInput
          label="Initial database"
          id="{p}Database"
          placeholder="If empty, it will match the normal user."
          readonly={started}
          disabled={!canUpdate}
          helper={started ? 'You can only change this in the database.' : undefined}
          bind:value={form.creds[`${p}_database`]}
          error={err(`${p}_database`)}
        />
      </div>
    </DatabaseSubsection>
  {:else if kind === 'clickhouse'}
    <DatabaseSubsection id="credentials-section" title="Credentials" helper="Keep these values aligned with the credentials configured inside ClickHouse.">
      {#if !started}
        <Callout type="warning" title="Verify the initial credentials">
          You can only change these credentials here before the first start. Later changes must be made inside the database.
        </Callout>
      {/if}
      <div class="{!started ? 'mt-4 ' : ''}grid gap-4 lg:grid-cols-2">
        <FormInput
          label={started ? 'Initial username' : 'Username'}
          id="clickhouseAdminUser"
          placeholder="If empty: clickhouse"
          readonly={started}
          required
          disabled={!canUpdate}
          bind:value={form.creds.clickhouse_admin_user}
          error={err('clickhouse_admin_user')}
        />
        {#if hidden}
          {@render hiddenInput('Password', 'clickhouseAdminPasswordHidden')}
        {:else}
          <FormInput
            label={started ? 'Initial password' : 'Password'}
            id="clickhouseAdminPassword"
            type="password"
            required
            readonly={started}
            helper={started ? 'You can only change this in the database.' : undefined}
            bind:value={form.creds.clickhouse_admin_password}
            error={err('clickhouse_admin_password')}
          />
        {/if}
      </div>
    </DatabaseSubsection>
  {/if}
{/snippet}

<div class={kind === 'postgresql' ? 'application-settings-form flex flex-col gap-6' : 'application-settings-form'}>
  <form
    class="flex flex-col gap-6"
    onsubmit={(e) => {
      e.preventDefault();
      submit();
    }}
  >
    <UnsavedBar {dirty} {saving} onsave={submit} onreset={() => ((form = JSON.parse(JSON.stringify(saved))), (error = null))} />

    {#if anonymousVolume}
      <Callout type="danger" title="Current data is not in the data volume">
        <p>
          Because of a bug in older Coolify versions, a file mount removed the data volume. This database keeps its current data in the unnamed Docker volume <code>{anonymousVolume}</code>. Starting or
          restarting it is blocked, because it would use the data volume and the current data would seem lost.
        </p>
        <p class="mt-2">
          An admin can click "Keep current data" to use this volume as the data volume. The previous data volume is not used anymore, but stays on the server. It can contain data from before the file
          mount was added.
        </p>
        {#if canUpdate}
          <div class="mt-3"><button type="button" class="button" onclick={() => (confirmKeep = true)}>Keep current data</button></div>
        {/if}
      </Callout>
    {/if}

    <SettingsSection id="database-details-section" title="Database details" description="Manage the identity, container image, and credentials of this {label} database.">
      {#snippet actions()}
        <button type="button" class="button" onclick={() => (details = true)}>Details</button>
      {/snippet}
      <div class="grid gap-4 lg:grid-cols-2">
        <FormInput label="Name" id="name" disabled={!canUpdate} bind:value={form.name} error={err('name')} />
        <FormInput label="Description" id="description" disabled={!canUpdate} bind:value={form.description} error={err('description')} />
        <div class="lg:col-span-2">
          <FormInput label="Image" id="image" required disabled={!canUpdate} helper={imageHelpers[kind]} bind:value={form.image} error={err('image')} />
        </div>
      </div>
      {@render credentials()}
    </SettingsSection>

    <SettingsSection id="access-section" title="Access" description="How applications and clients connect to this database, and whether it is reachable from the internet.">
      {#snippet actions()}
        {#if isPublic}
          <button type="button" class="button" onclick={() => (proxyLogs = true)}>View logs</button>
        {/if}
      {/snippet}
      <DatabaseSubsection id="connection-section" title="Connection" helper="Use these URLs to connect to the database." first>
        <div class="space-y-5">
          {#if hidden}
            <FormInput label="{urlLabels[kind]} URL (internal)" id="dbUrlHidden" disabled value="Hidden (only admins can view)" />
            <FormInput label="{urlLabels[kind]} URL (public)" id="dbUrlPublicHidden" disabled value="Hidden (only admins can view)" />
          {:else if lockHidesUrl}
            <FormInput
              label="{urlLabels[kind]} URL (internal)"
              id="dbUrlLocked"
              disabled
              value="Locked (shown once)"
              helper="The password variable is locked, so the URL that contains it is not shown."
            />
          {:else}
            <FormInput label="{urlLabels[kind]} URL (internal)" id="dbUrl" helper={urlHelper} type="password" readonly value={record.internal_db_url ?? ''} />
            {#if record.external_db_url}
              <FormInput label="{urlLabels[kind]} URL (public)" id="dbUrlPublic" helper={urlHelper} type="password" readonly value={record.external_db_url} />
            {:else if publicPlaceholder}
              <FormInput label="{urlLabels[kind]} URL (public)" id="dbUrlPublic" readonly value="Starting the database will generate this." />
            {/if}
          {/if}

          {#if supportsSsl}
            <div class="border-t border-neutral-200 pt-5 dark:border-white/[0.06]">
              <div class="mb-4 flex items-center justify-between gap-3">
                <div>
                  <h3 class="text-sm font-semibold text-black dark:text-fg">SSL configuration</h3>
                  <p class="mt-1 text-xs text-neutral-500 dark:text-fg-dim">Encryption settings can only be changed while the database is stopped.</p>
                </div>
                {#if enableSsl && extras.certificate_valid_until}
                  <button type="button" class="button" onclick={() => (confirmRegenerate = true)}>Regenerate SSL Certificates</button>
                {/if}
              </div>
              {#if enableSsl && extras.certificate_valid_until}
                <div class="mb-4 text-sm text-neutral-600 dark:text-fg-dim">
                  Valid until:
                  {#if extras.certificate_state === 'expired'}
                    <span class="text-red-500">{extras.certificate_valid_until} - Expired</span>
                  {:else if extras.certificate_state === 'soon'}
                    <span class="text-red-500">{extras.certificate_valid_until} - Expiring soon</span>
                  {:else}
                    <span>{extras.certificate_valid_until}</span>
                  {/if}
                </div>
              {/if}
              <div class="grid gap-4 sm:grid-cols-2">
                <Listbox
                  id="enableSsl"
                  label="SSL"
                  disabled={!exited || !canUpdate}
                  bind:value={enableSsl}
                  onchange={saveSsl}
                  options={[
                    { value: true, label: 'Enabled' },
                    { value: false, label: 'Disabled' },
                  ]}
                />
                {#if sslModes}
                  <Listbox
                    id="sslMode"
                    label="SSL mode"
                    helper={sslModes.helper}
                    disabled={!enableSsl || !exited || !canUpdate}
                    bind:value={sslMode}
                    onchange={async () => {
                      // `onChange="instantSaveSSL"` and `updatedSslMode` both save.
                      await saveSsl();
                      await saveSsl();
                    }}
                    options={sslModes.options}
                  />
                {/if}
              </div>
            </div>
          {/if}
        </div>
      </DatabaseSubsection>
      <DatabaseSubsection id="public-access-section" title="Public access" helper="Expose this database through the managed TCP proxy." class="relative">
        <div class="grid gap-4 lg:grid-cols-2">
          <Listbox
            id="isPublic"
            label="Access"
            disabled={!canUpdate}
            bind:value={isPublic}
            onchange={(v: boolean) => savePublic(v)}
            options={[
              { value: false, label: 'Private' },
              {
                value: true,
                label: str(saved.publicPort) === '' ? 'Public through TCP proxy (set public port first)' : 'Public through TCP proxy',
                disabled: str(saved.publicPort) === '',
              },
            ]}
          />
          <FormInput
            type="number"
            placeholder={port}
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
            placeholder="3000:{port}"
            id="portsMappings"
            label="Port mappings"
            helper="Comma-separated host-to-container mappings, for example 3000:{port}."
            disabled={!canUpdate}
            bind:value={form.portsMappings}
            error={err('ports_mappings')}
          />
        </div>
      </DatabaseSubsection>
    </SettingsSection>

    <SettingsSection
      id="container-section"
      title="Container"
      description={conf ? 'Docker options, configuration, and log forwarding for the database container.' : 'Docker options and log forwarding for the database container.'}
    >
      <FormInput
        helper="Add supported docker run options used when the container starts. Unsupported options can interfere with coolify-rs automation."
        placeholder="--cap-add SYS_ADMIN --device=/dev/fuse"
        id="customDockerRunOptions"
        label="Custom Docker options"
        disabled={!canUpdate}
        bind:value={form.customDockerRunOptions}
        error={err('custom_docker_run_options')}
      />
      {#if conf}
        <DatabaseSubsection id="configuration-section" title="Configuration" helper={conf.section}>
          {#if hidden}
            <FormInput label={conf.label} id="confHidden" disabled value="Hidden (only admins can view)" />
          {:else}
            <FormTextarea
              label={conf.label}
              rows={10}
              id="conf"
              helper={conf.helper}
              placeholder={conf.placeholder}
              bind:value={form.conf}
              error={err(conf.column)}
            />
          {/if}
        </DatabaseSubsection>
      {/if}
      <DatabaseSubsection id="log-delivery-section" title="Log delivery" helper="Forward container logs to the drain configured on the server.">
        <Listbox
          id="isLogDrainEnabled"
          label="Log drain"
          disabled={!canUpdate}
          bind:value={logDrain}
          onchange={(v: boolean) => saveLogDrain(v)}
          options={[
            { value: false, label: 'Do not forward logs' },
            { value: true, label: 'Forward logs to the server drain' },
          ]}
        />
      </DatabaseSubsection>
    </SettingsSection>
  </form>

  {#if kind === 'postgresql'}
    <SettingsSection id="initialization-scripts-section" title="Initialization scripts" description="Run SQL files in order when PostgreSQL initializes for the first time." flush>
      {#snippet actions()}
        {#if canUpdate}
          <button type="button" class="button" onclick={() => (addingScript = true)}>+ Add</button>
        {/if}
      {/snippet}
      <div class="divide-y divide-neutral-200 dark:divide-border-subtle">
        {#if hidden}
          <Empty title="Hidden (only admins can view)" description="Initialization scripts can contain credentials." />
        {:else if scripts.length}
          {#each scripts as script, i (script.original)}
            <!-- livewire/project/database/init-script.blade.php (Project\Database\InitScript) -->
            <form
              class="application-settings-form flex flex-col gap-4 px-4 py-4"
              onsubmit={(e) => {
                e.preventDefault();
                saveScript(script);
              }}
            >
              <div class="flex items-end gap-2">
                <FormInput id="init-script-{i}" label="Filename" bind:value={script.filename} />
                <button type="button" class="button button-error" onclick={() => (deletingScript = script.original)}>Delete</button>
              </div>
              <FormTextarea id="init-script-content-{i}" label="Content" rows={12} bind:value={script.content} />
              {#if script.filename !== script.original || script.content !== (record.init_scripts ?? []).find((s) => s.filename === script.original)?.content}
                <div class="flex justify-end"><button type="submit" class="button">Save</button></div>
              {/if}
            </form>
          {/each}
        {:else}
          <Empty title="No initialization scripts" description="Add a SQL file to run during the first PostgreSQL initialization." />
        {/if}
      </div>
    </SettingsSection>
  {/if}
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

{#if confirmRegenerate}
  <ConfirmModal
    title="Regenerate SSL Certificates"
    actions={['The SSL certificate of this database will be regenerated.', 'You must restart the database after regenerating the certificate to start using the new certificate.']}
    confirmWithText={false}
    confirmWithPassword={false}
    onconfirm={async () => {
      await generalUi.regenerateSsl(record.uuid);
      await onsaved();
    }}
    onclose={() => (confirmRegenerate = false)}
  />
{/if}

{#if confirmKeep && anonymousVolume}
  <ConfirmModal
    title="Keep current data?"
    actions={[
      `Use the unnamed volume ${anonymousVolume} as the data volume of this database.`,
      'Starts and restarts are allowed again and keep the current data.',
      'The previous data volume stays on the server and is not used anymore.',
    ]}
    confirmWithText={false}
    confirmWithPassword={false}
    step2ButtonText="Keep current data"
    onconfirm={async () => {
      await generalUi.keepAnonymousVolume(record.uuid);
      anonymousVolume = null;
      await onsaved();
    }}
    onclose={() => (confirmKeep = false)}
  />
{/if}

{#if deletingScript !== null}
  {@const name = deletingScript}
  <ConfirmModal
    title="Delete initialization script?"
    actions={['Permanently delete this initialization script from the database and server.', 'Redeployments that depend on this script may fail.']}
    confirmationText={name}
    confirmationLabel="Enter the initialization script name to confirm."
    shortConfirmationLabel="Script name"
    confirmWithPassword={false}
    step2ButtonText="Permanently delete"
    onconfirm={async () => {
      await deleteScript(name);
    }}
    onclose={() => (deletingScript = null)}
  />
{/if}

{#if addingScript}
  <Modal title="New initialization script" onclose={() => (addingScript = false)}>
    <form class="flex w-full flex-col gap-4" onsubmit={addScript}>
      <FormInput placeholder="create_test_db.sql" id="new_filename" label="Filename" required bind:value={newFilename} error={scriptError?.field('new_filename')} />
      <FormTextarea rows={16} placeholder="CREATE DATABASE test;" id="new_content" label="Content" required bind:value={newContent} error={scriptError?.field('new_content')} />
      <div class="flex justify-end border-t border-neutral-200 pt-4 dark:border-white/[0.08]">
        <button type="submit" class="button">Add script</button>
      </div>
    </form>
  </Modal>
{/if}

{#if proxyLogs}
  <ProcessDialog title="Proxy logs" closeWithX size="xl" onclose={() => (proxyLogs = false)}>
    <LogViewer containerName="{record.uuid}-proxy" fetchLogs={fetchProxyLogs} collapsible={false} />
  </ProcessDialog>
{/if}
