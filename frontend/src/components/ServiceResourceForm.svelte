<script lang="ts">
  // Port of upstream livewire/project/service/index.blade.php (embedded) and
  // advanced-settings.blade.php: the settings of a compose application or database.
  import { untrack } from 'svelte';
  import Callout from './Callout.svelte';
  import ConfirmModal from './ConfirmModal.svelte';
  import FormInput from './FormInput.svelte';
  import Listbox from './Listbox.svelte';
  import Modal from './Modal.svelte';
  import Reicon from './Reicon.svelte';
  import ServiceDomainConflict from './ServiceDomainConflict.svelte';
  import StatusBadge from './StatusBadge.svelte';
  import { ApiError } from '../lib/api';
  import { headline, servicesApi, type ServiceResourceDetails } from '../lib/servicesApi';
  import { link } from '../lib/router.svelte';
  import { notify } from '../lib/toast.svelte';

  let {
    serviceUuid,
    resource,
    canUpdate,
    serviceBase,
    onsaved,
    ondone,
  }: {
    serviceUuid: string;
    resource: ServiceResourceDetails;
    canUpdate: boolean;
    serviceBase: string;
    onsaved: () => Promise<void>;
    /** The resource is gone (deleted or converted): `redirectRoute` to the configuration page. */
    ondone: () => Promise<void>;
  } = $props();

  const isApplication = $derived(resource.kind === 'application');
  const displayName = $derived(headline(resource.name));
  const IMAGE_HELPER =
    "You can change the image you would like to deploy.<br><br><span class='dark:text-warning'>WARNING. You could corrupt your data. Only do it if you know what you are doing.</span>";

  const initial = untrack(() => resource);
  let humanName = $state(initial.human_name ?? '');
  let description = $state(initial.description ?? '');
  let image = $state(initial.image ?? '');
  let url = $state(initial.url ?? '');
  let excludeFromStatus = $state(initial.exclude_from_status);
  let isLogDrainEnabled = $state(initial.is_log_drain_enabled);
  let isGzipEnabled = $state(initial.is_gzip_enabled ?? true);
  let isStripprefixEnabled = $state(initial.is_stripprefix_enabled ?? true);
  let maxRestartCount = $state(String(initial.max_restart_count ?? 0));
  let resolverMode = $state<'server' | 'none' | 'custom'>(initial.certificate_resolver_mode ?? 'server');
  let resolverName = $state(initial.certificate_resolver_name ?? '');
  let publicPort = $state(initial.public_port === null || initial.public_port === undefined ? '' : String(initial.public_port));
  let publicTimeout = $state(initial.public_port_timeout === null || initial.public_port_timeout === undefined ? '' : String(initial.public_port_timeout));
  let isPublic = $state(initial.is_public ?? false);
  let dbUrl = $state(initial.db_url_public ?? '');

  let saving = $state(false);
  let error = $state<ApiError | null>(null);
  let conflicts = $state<{ domain: string; resource_name: string; resource_type: string; resource_link?: string | null }[] | null>(null);
  let requiredPort = $state<number | null>(null);
  /** `$requestedPort`: the port of the domain that does not use the required port. */
  let requestedPort = $state<number | null>(null);
  let forceDomains = false;
  let forcePort = false;
  let confirmDelete = $state(false);
  let confirmConvert = $state(false);

  const pocketbase = $derived((resource.image ?? '').includes('pocketbase'));
  const message = (e: unknown) => (e instanceof ApiError ? e.message : String(e));

  async function submit() {
    saving = true;
    error = null;
    try {
      const r = await servicesApi.saveResource(serviceUuid, resource.uuid, {
        human_name: humanName,
        description,
        image,
        exclude_from_status: excludeFromStatus,
        is_log_drain_enabled: isLogDrainEnabled,
        ...(isApplication
          ? {
              url,
              is_gzip_enabled: isGzipEnabled,
              is_stripprefix_enabled: isStripprefixEnabled,
              certificate_resolver_mode: resolverMode,
              certificate_resolver_name: resolverName,
              max_restart_count: maxRestartCount,
              force_domains: forceDomains,
              force_port: forcePort,
            }
          : { public_port: publicPort, public_port_timeout: publicTimeout }),
      });
      forceDomains = false;
      forcePort = false;
      if (r.domain_conflicts) {
        conflicts = r.domain_conflicts;
        return;
      }
      if (r.port_warning) {
        requiredPort = r.port_warning;
        requestedPort = r.requested_port ?? null;
        return;
      }
      if (r.warning) notify.warning(r.warning);
      else if (r.message) notify.success(r.message);
      await onsaved();
    } catch (e) {
      error = e instanceof ApiError ? e : new ApiError(0, String(e));
      if (!Object.keys(error.errors).length) notify.error(error.message);
    } finally {
      saving = false;
    }
  }

  async function setPublic(v: boolean) {
    try {
      const r = await servicesApi.publicAccess(serviceUuid, resource.uuid, {
        is_public: v,
        public_port: publicPort,
        public_port_timeout: publicTimeout,
      });
      isPublic = v;
      notify.success(r.message);
      await onsaved();
      const fresh = await servicesApi.resource(serviceUuid, resource.uuid);
      dbUrl = fresh.resource.db_url_public ?? '';
    } catch (e) {
      notify.error(message(e));
    }
  }

  async function remove(password: string) {
    try {
      const r = await servicesApi.deleteResource(serviceUuid, resource.uuid, password);
      if (r.warning) notify.warning(r.message);
      else notify.success(r.message);
      await ondone();
    } catch (e) {
      if (e instanceof ApiError && e.status === 422) return e.message;
      notify.error(message(e));
    }
  }

  async function convert() {
    try {
      await servicesApi.convertResource(serviceUuid, resource.uuid);
      await ondone();
    } catch (e) {
      notify.error(message(e));
    }
  }
</script>

<form
  class="space-y-6"
  onsubmit={(e) => {
    e.preventDefault();
    submit();
  }}
>
  {#if isApplication}
    <div class="space-y-4">
      <div class="grid gap-4 sm:grid-cols-2">
        <FormInput id="humanName" label="Name" placeholder="Human readable name" disabled={!canUpdate} bind:value={humanName} />
        <FormInput id="description" label="Description" disabled={!canUpdate} bind:value={description} />
      </div>
      <div class="grid gap-4 sm:grid-cols-2">
        {#if !resource.hides_domains}
          <div data-domain-summary class="rounded-lg border border-neutral-200 p-4 dark:border-white/[0.08]">
            <div class="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
              <p class="text-sm text-neutral-500 dark:text-fg-dim">
                {#if !resource.domain_count}
                  No domains set.
                {:else if resource.domain_count === 1}
                  1 domain set.
                {:else}
                  {resource.domain_count} domains set.
                {/if}
              </p>
              <a class="button shrink-0" href="{serviceBase}/domains" use:link>
                <Reicon name="globe" class="size-4" />
                Manage domains
              </a>
            </div>
          </div>
        {/if}
        <FormInput id="image" label="Image" helper={IMAGE_HELPER} disabled={!canUpdate} bind:value={image} error={error?.field('image')} />
      </div>
    </div>
  {:else}
    <div class="space-y-5">
      <div class="grid gap-4 sm:grid-cols-2">
        <FormInput id="humanName" label="Name" placeholder="Name" disabled={!canUpdate} bind:value={humanName} />
        <FormInput id="description" label="Description" disabled={!canUpdate} bind:value={description} />
        <div class="sm:col-span-2">
          <FormInput id="image" label="Image" required helper={IMAGE_HELPER} disabled={!canUpdate} bind:value={image} error={error?.field('image')} />
        </div>
      </div>
      <div class="border-t border-neutral-200 pt-5 dark:border-white/[0.06]">
        <div class="mb-4 flex items-center justify-between gap-2">
          <h3 class="text-sm font-semibold text-black dark:text-fg">Public access</h3>
        </div>
        <div class="space-y-4">
          <div class="flex flex-col gap-2 sm:flex-row sm:items-end">
            <div class="w-full sm:max-w-xs">
              <FormInput
                id="publicPort"
                type="number"
                label="Public Port"
                placeholder="5432"
                disabled={!canUpdate || isPublic}
                bind:value={publicPort}
                error={error?.field('public_port')}
              />
            </div>
            <div class="flex shrink-0 flex-wrap items-center gap-2">
              {#if isPublic}
                <StatusBadge label="Public" type="success" />
                <button type="button" class="button" disabled={!canUpdate} onclick={() => setPublic(false)}>Make private</button>
              {:else}
                <button type="button" class="button" disabled={!canUpdate || !publicPort.trim()} onclick={() => setPublic(true)}>Make publicly available</button>
              {/if}
            </div>
          </div>
          {#if isPublic && dbUrl}
            <FormInput
              id="db_url_public"
              label="Database IP:PORT (public)"
              helper="Your credentials are available in your environment variables."
              type="password"
              readonly
              value={dbUrl}
            />
          {/if}
        </div>
      </div>
    </div>
  {/if}

  <!-- advanced-settings.blade.php -->
  <div class="grid gap-4 sm:grid-cols-2">
    {#if isApplication}
      <Listbox
        id="isGzipEnabled"
        label="Gzip compression"
        helper={pocketbase ? 'PocketBase keeps compression disabled so server-sent events continue to work.' : undefined}
        disabled={pocketbase || !canUpdate}
        bind:value={isGzipEnabled}
        options={[
          { value: true, label: 'Enabled' },
          { value: false, label: 'Disabled' },
        ]}
      />
      <Listbox
        id="isStripprefixEnabled"
        label="Path prefixes"
        disabled={!canUpdate}
        bind:value={isStripprefixEnabled}
        options={[
          { value: true, label: 'Strip prefixes' },
          { value: false, label: 'Keep prefixes' },
        ]}
      />
    {/if}
    <Listbox
      id="excludeFromStatus"
      label="Service status"
      disabled={!canUpdate}
      bind:value={excludeFromStatus}
      options={[
        { value: false, label: 'Include in status' },
        { value: true, label: 'Exclude from status' },
      ]}
    />
    <Listbox
      id="isLogDrainEnabled"
      label="Log drain"
      disabled={!canUpdate}
      bind:value={isLogDrainEnabled}
      options={[
        { value: true, label: 'Send logs to drain' },
        { value: false, label: 'Do not drain logs' },
      ]}
    />
    {#if isApplication}
      <FormInput
        id="maxRestartCount"
        type="number"
        min="0"
        label="Max restart count"
        helper="Maximum number of Docker restarts before coolify-rs stops this container. Set to 0 to disable the limit. Docker counts expected and unexpected restarts."
        disabled={!canUpdate}
        bind:value={maxRestartCount}
        error={error?.field('max_restart_count')}
      />
      {#if resource.show_certificate_resolver}
        <Listbox
          id="certificateResolverMode"
          label="Certificate resolver"
          helper="Sets the Traefik certificate resolver for this service's HTTPS routes. Choose None for domains that use certificates from Traefik's dynamic configuration."
          disabled={!canUpdate}
          bind:value={resolverMode}
          options={[
            { value: 'server', label: `Server setting (${resource.server_certificate_resolver})` },
            { value: 'none', label: 'None (use your own certificates)' },
            { value: 'custom', label: 'Custom resolver' },
          ]}
        />
        {#if resolverMode === 'custom'}
          <FormInput
            id="certificateResolverName"
            label="Certificate resolver name"
            placeholder="cloudflare"
            required
            helper="Name of a certificate resolver defined in the Traefik configuration of this server."
            disabled={!canUpdate}
            bind:value={resolverName}
            error={error?.field('certificate_resolver_name')}
          />
        {/if}
      {/if}
    {/if}
  </div>

  <div data-service-resource-actions class="flex flex-wrap items-center justify-between gap-3 border-t border-neutral-200 pt-5 dark:border-white/[0.08]">
    <div>
      {#if canUpdate}
        <button type="button" class="button button-error" onclick={() => (confirmDelete = true)}>Delete</button>
      {/if}
    </div>
    <div class="ml-auto flex items-center gap-2">
      {#if canUpdate}
        <button type="button" class="button" onclick={() => (confirmConvert = true)}>{isApplication ? 'Convert to Database' : 'Convert to Application'}</button>
        <button type="submit" class="button button-highlighted" disabled={saving}>Save changes</button>
      {/if}
    </div>
  </div>
</form>

{#if conflicts}
  <ServiceDomainConflict
    {conflicts}
    consequences={[
      'Only one service will be accessible at this domain',
      'The routing behavior will be unpredictable',
      'You may experience service disruptions',
      'SSL certificates might not work correctly',
    ]}
    onproceed={() => {
      conflicts = null;
      forceDomains = true;
      submit();
    }}
    onclose={() => (conflicts = null)}
  />
{/if}

{#if requiredPort !== null}
  <Modal title="Use a different port?" onclose={() => (requiredPort = null)}>
    <Callout type="warning" title="Port requirement" class="mb-4">
      This service requires port <strong>{requiredPort}</strong> to function correctly. One or more of your domains use port <strong>{requestedPort}</strong>.
    </Callout>
    <Callout type="danger" title="What will happen if you continue?" class="mb-4">
      <ul class="mt-2 ml-4 list-disc">
        <li>The service may become unreachable</li>
        <li>The proxy may not be able to route traffic correctly</li>
        <li>Environment variables may not be generated properly</li>
        <li>The service may fail to start or function</li>
      </ul>
    </Callout>
    <div class="mt-4 flex flex-wrap justify-end gap-2 border-t border-neutral-200 pt-4 dark:border-white/[0.08]">
      <button
        type="button"
        class="button button-error w-auto"
        onclick={() => {
          requiredPort = null;
          forcePort = true;
          submit();
        }}>Use port {requestedPort} anyway</button
      >
      <button
        type="button"
        class="button w-auto"
        onclick={() => {
          requiredPort = null;
          url = resource.url ?? '';
        }}>Keep port {requiredPort}</button
      >
    </div>
  </Modal>
{/if}

{#if confirmDelete}
  <ConfirmModal
    title={isApplication ? 'Confirm Service Application Deletion?' : 'Confirm Service Database Deletion?'}
    actions={[isApplication ? 'The selected service application container will be stopped and permanently deleted.' : 'The selected service database container will be stopped and permanently deleted.']}
    confirmationText={displayName}
    confirmationLabel={isApplication
      ? 'Please confirm the execution of the actions by entering the Service Application Name below'
      : 'Please confirm the execution of the actions by entering the Service Database Name below'}
    shortConfirmationLabel={isApplication ? 'Service Application Name' : 'Service Database Name'}
    confirmWithPassword
    onconfirm={remove}
    onclose={() => (confirmDelete = false)}
  />
{/if}

{#if confirmConvert}
  <ConfirmModal
    title={isApplication ? 'Convert to Database' : 'Convert to Application'}
    actions={[isApplication ? 'The selected resource will be converted to a service database.' : 'The selected resource will be converted to an application.']}
    confirmationText={displayName}
    confirmationLabel={isApplication
      ? 'Please confirm the execution of the actions by entering the Service Application Name below'
      : 'Please confirm the execution of the actions by entering the Service Database Name below'}
    shortConfirmationLabel={isApplication ? 'Service Application Name' : 'Service Database Name'}
    confirmWithPassword
    onconfirm={async () => {
      await convert();
    }}
    onclose={() => (confirmConvert = false)}
  />
{/if}
