<script lang="ts">
  // Port of upstream livewire/server/show.blade.php (General). Validating the
  // connection, fetching server details and cloud provider links need SSH or
  // provider APIs, which coolify-rs does not have yet; the localhost server's
  // page is not ported.
  import Reicon from '../components/Reicon.svelte';
  import Empty from '../components/Empty.svelte';
  import FormInput from '../components/FormInput.svelte';
  import Listbox from '../components/Listbox.svelte';
  import UnsavedBar from '../components/UnsavedBar.svelte';
  import StatusBadge from '../components/StatusBadge.svelte';
  import SettingsSection from '../components/SettingsSection.svelte';
  import ConfirmModal from '../components/ConfirmModal.svelte';
  import ServerLayout from '../components/ServerLayout.svelte';
  import Unported from './Unported.svelte';
  import { api, serversUi, ApiError, type ServerDetail, type ServerFacts } from '../lib/api';
  import { loadServerContext } from '../lib/serverPage.svelte';
  import { session } from '../lib/session.svelte';
  import { notify } from '../lib/toast.svelte';

  let { uuid }: { uuid: string } = $props();

  type Form = { name: string; description: string; ip: string; user: string; port: string; connectionTimeout: string; serverTimezone: string; wildcardDomain: string };

  let facts = $state<ServerFacts | null>(null);
  let missing = $state(false);
  let form = $state<Form | null>(null);
  let saved = $state<Form | null>(null);
  let role = $state<string>('both');
  // The saved role; `role` follows the picker.
  let committedRole = $state<string>('both');
  let timezones = $state<string[]>([]);
  let saving = $state(false);
  let error = $state<ApiError | null>(null);
  let confirmBoth = $state(false);

  const canEdit = $derived(session.isAdmin);
  const dirty = $derived(!!form && !!saved && JSON.stringify(form) !== JSON.stringify(saved));

  async function load() {
    const ctx = await loadServerContext(uuid, 'general');
    if (!ctx) {
      missing = true;
      return;
    }
    facts = ctx.facts;
    document.title = `${ctx.facts.name.slice(0, 24)} | Server | coolify-rs`;
    if (ctx.facts.is_localhost) return;
    const d = await api.get<ServerDetail>(`/servers/${encodeURIComponent(uuid)}`);
    const f: Form = {
      name: d.name,
      description: d.description ?? '',
      ip: d.ip,
      user: d.user,
      port: String(d.port),
      connectionTimeout: String(d.settings.connection_timeout),
      serverTimezone: d.settings.server_timezone,
      wildcardDomain: d.settings.wildcard_domain ?? '',
    };
    saved = { ...f };
    form = { ...f };
    role = d.settings.server_role ?? (d.settings.is_build_server ? 'build' : 'both');
    committedRole = role;
  }
  $effect(() => {
    void uuid;
    load();
  });
  serversUi.timezones().then((t) => (timezones = t));

  async function save() {
    if (!form) return;
    saving = true;
    error = null;
    try {
      await api.patch(`/servers/${encodeURIComponent(uuid)}`, {
        name: form.name,
        description: form.description,
        ip: form.ip,
        user: form.user,
        port: form.port === '' ? null : Number(form.port),
        connection_timeout: form.connectionTimeout === '' ? null : Number(form.connectionTimeout),
        server_timezone: form.serverTimezone,
        wildcard_domain: form.wildcardDomain === '' ? null : form.wildcardDomain,
      });
      notify.ok('Server settings updated.');
      saved = { ...form };
      await load();
    } catch (e) {
      error = e instanceof ApiError ? e : new ApiError(0, String(e));
      if (!Object.keys(error.errors).length) notify.error(error.message);
    } finally {
      saving = false;
    }
  }

  async function saveRole(next: string) {
    try {
      await api.patch(`/servers/${encodeURIComponent(uuid)}`, { server_role: next });
      role = committedRole = next;
      notify.ok('Server settings updated.');
    } catch (e) {
      role = committedRole;
      notify.error(e instanceof ApiError ? (Object.values(e.errors).flat()[0] ?? e.message) : String(e));
    }
  }

  /** `requestServerRoleChange`: switching to "both" from another role asks first. */
  function requestRole(next: string) {
    if (next === 'both' && committedRole !== 'both') {
      role = committedRole;
      confirmBoth = true;
      return;
    }
    saveRole(next);
  }

  const err = (k: string) => error?.field(k);
</script>

{#if missing}
  <div class="application-settings-form w-full">
    <Empty title="Server not found" description="It may have been deleted, or it belongs to another team." iconName="servers" size="lg" />
  </div>
{:else if facts}
  <ServerLayout name={facts.name}>
    {#if facts.is_localhost}
      <Unported description="The settings of the server Coolify runs on are not ported to coolify-rs yet." />
    {:else if form}
      <form
        onsubmit={(e) => {
          e.preventDefault();
          save();
        }}
        class="application-settings-form flex flex-col gap-6"
      >
        <UnsavedBar {dirty} {saving} onsave={save} onreset={() => ((form = { ...saved! }), (error = null))} />
        <SettingsSection title="Connection" description="Configure how Coolify identifies, reaches, and validates this server.">
          {#snippet badges()}
            {#if facts?.transferred}
              <StatusBadge label="Transferred away" type="warning" />
            {:else}
              <StatusBadge label={facts?.functional ? 'Ready' : 'Validation required'} type={facts?.functional ? 'success' : 'warning'} />
            {/if}
          {/snippet}
          {#snippet actions()}
            <button
              type="button"
              class="button"
              {...facts?.functional ? {} : { ishighlighted: '' }}
              onclick={() => notify.info('Not available in coolify-rs yet', 'Validating a server needs SSH, which coolify-rs does not run yet.')}
            >
              <Reicon name={facts?.functional ? 'refresh' : 'alert-circle'} class="size-3.5" />
              {facts?.functional ? 'Revalidate connection' : 'Validate connection'}
            </button>
          {/snippet}

          <div class="grid gap-4 sm:grid-cols-2">
            <FormInput id="name" label="Name" required disabled={!canEdit} bind:value={form.name} error={err('name')} />
            <FormInput id="description" label="Description" disabled={!canEdit} bind:value={form.description} error={err('description')} />
          </div>
          <div class="mt-4 grid gap-4 lg:grid-cols-3">
            <FormInput
              type="password"
              id="ip"
              label="IP address or domain"
              helper="Enter a hostname or IP address without http:// or https://."
              required
              disabled={!canEdit || facts.transferred}
              bind:value={form.ip}
              error={err('ip')}
            />
            <FormInput id="user" label="SSH user" required disabled={!canEdit || facts.transferred} bind:value={form.user} error={err('user')} />
            <FormInput type="number" id="port" label="SSH port" required disabled={!canEdit || facts.transferred} bind:value={form.port} error={err('port')} />
          </div>
          <div class="mt-4 grid gap-4 lg:grid-cols-3">
            <FormInput
              type="number"
              id="connectionTimeout"
              label="Connection timeout"
              helper="Seconds to wait before an SSH connection fails."
              min="1"
              max="300"
              required
              disabled={!canEdit}
              bind:value={form.connectionTimeout}
              error={err('connection_timeout')}
            />
            <Listbox
              searchable
              id="serverTimezone"
              label="Server timezone"
              helper="Used for backups, cron jobs, and displayed timestamps."
              searchPlaceholder="Search timezones"
              searchEmptyText="No matching timezone"
              disabled={!canEdit}
              bind:value={form.serverTimezone}
              options={timezones.map((t) => ({ value: t, label: t }))}
            />
            {#if !facts.is_swarm_worker && role !== 'build'}
              <FormInput
                placeholder="https://example.com"
                id="wildcardDomain"
                label="Wildcard domain"
                helper="New resources can receive generated subdomains from this domain."
                disabled={!canEdit}
                bind:value={form.wildcardDomain}
                error={err('wildcard_domain')}
              />
            {/if}
          </div>
          <div class="mt-4 border-t border-neutral-200 pt-4 dark:border-white/[0.08]">
            <Listbox
              id="serverRole"
              label="Server role"
              helper="Builds can use large amounts of CPU and memory. Deployments on the same server can become slow or unreachable during a build. GitHub Actions runners need the Builds only or the Deployments and builds role."
              disabled={!canEdit}
              bind:value={role}
              onchange={(v) => requestRole(v)}
              options={[
                { value: 'deployment', label: 'Deployments only', description: 'Runs your resources. Images are built on a build server.' },
                { value: 'build', label: 'Builds only', description: 'Builds images for other servers and runs GitHub Actions runners.' },
                { value: 'both', label: 'Deployments and builds', description: 'Builds and runs your resources on this server. Can also run GitHub Actions runners.' },
              ]}
            />
          </div>
        </SettingsSection>

        <SettingsSection title="Server overview" description="Provider state, operating system, and hardware details.">
          {#snippet actions()}
            <span class="relative inline-flex" title={facts?.functional ? undefined : 'Validate the connection first.'}>
              <button type="button" class="button" disabled={!facts?.functional}>
                <Reicon name="refresh" class="size-3.5" />
                Fetch server details
              </button>
            </span>
          {/snippet}
          <div class="flex items-start gap-3">
            <div class="flex size-9 shrink-0 items-center justify-center rounded-lg bg-neutral-100 text-neutral-600 dark:bg-white/[0.06] dark:text-fg-dim">
              <Reicon name="servers" class="size-4.5" />
            </div>
            <div class="min-w-0">
              <p class="truncate text-sm font-medium text-neutral-950 dark:text-fg">{facts.name}</p>
              <p class="mt-1 text-xs leading-5 text-neutral-500 dark:text-fg-dim">
                {#if facts.transferred}
                  This server was migrated away from this Coolify instance and cannot be managed here.
                {:else if facts.functional}
                  The server is reachable, validated, and ready to host resources.
                {:else}
                  Validate the SSH connection before using this server.
                {/if}
              </p>
            </div>
          </div>
        </SettingsSection>
      </form>
    {/if}
  </ServerLayout>
{/if}

{#if confirmBoth}
  <ConfirmModal
    title="Use this server for deployments and builds?"
    confirmWithText={false}
    step2ButtonText="Enable deployments and builds"
    warningMessage="Builds can use a large amount of CPU and memory. During a build, deployed resources on this server can become slow or unreachable."
    actions={['Enable builds on this deployment server.']}
    onconfirm={() => saveRole('both')}
    onclose={() => (confirmBoth = false)}
  />
{/if}
