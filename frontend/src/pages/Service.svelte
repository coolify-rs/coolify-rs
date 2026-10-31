<script lang="ts">
  // Port of upstream livewire/project/service/configuration.blade.php (+
  // Project\Service\Configuration). `Index::mount` redirects `/{stack_service_uuid}`
  // to this page: a compose resource is edited in the modal of its card.
  // `Project/Shared/*` sections not ported yet stay placeholders.
  import Empty from '../components/Empty.svelte';
  import EnvironmentVariables from '../components/shared/EnvironmentVariables.svelte';
  import RuntimeLogs from '../components/shared/RuntimeLogs.svelte';
  import SecretManagerLinks from '../components/shared/SecretManagerLinks.svelte';
  import Tags from '../components/shared/Tags.svelte';
  import Webhooks from '../components/shared/Webhooks.svelte';
  import ServiceDomains from '../components/ServiceDomains.svelte';
  import Reicon from '../components/Reicon.svelte';
  import ServiceHeading from '../components/ServiceHeading.svelte';
  import ServiceResourceCard from '../components/ServiceResourceCard.svelte';
  import ServiceSharedPlaceholder from '../components/ServiceSharedPlaceholder.svelte';
  import ResourceOperations from '../components/shared/ResourceOperations.svelte';
  import Maintenance from '../components/shared/Maintenance.svelte';
  import FileBrowser from '../components/shared/FileBrowser.svelte';
  import DatabaseDanger from '../components/DatabaseDanger.svelte';
  import ServiceStackForm from '../components/ServiceStackForm.svelte';
  import ServiceStorage from '../components/ServiceStorage.svelte';
  import SettingsSection from '../components/SettingsSection.svelte';
  import ViewToggle from '../components/ViewToggle.svelte';
  import Unported from './Unported.svelte';
  import { ApiError, uiApi, dbUi } from '../lib/api';
  import { headline, servicesApi, type ServicePage, type ServiceStoragePart } from '../lib/servicesApi';
  import { SERVICE_SECTIONS, serviceNavContext } from '../lib/serviceMenu';
  import { setProjectCrumbs } from '../lib/crumbs';
  import { strnatcmp } from '../lib/natsort';
  import { page } from '../lib/page.svelte';
  import { router } from '../lib/router.svelte';

  let { uuid, env, service, section }: { uuid: string; env: string; service: string; section: string } = $props();

  const base = $derived(`/project/${uuid}/environment/${env}/service/${service}`);
  const isSection = $derived(section === 'general' || SERVICE_SECTIONS.includes(section));

  let data = $state<ServicePage | null>(null);
  let storages = $state<ServiceStoragePart[] | null>(null);
  let resources = $state<{ label: string; href: string; active: boolean }[]>([]);
  let missing = $state(false);

  const viewKey = 'service-compose-resources-view';
  function readView(): string {
    try {
      return localStorage.getItem(viewKey) || 'table';
    } catch {
      return 'table';
    }
  }
  let viewMode = $state(readView());
  $effect(() => {
    try {
      localStorage.setItem(viewKey, viewMode);
    } catch {
      /* per-viewer convenience only */
    }
  });

  const hasParts = $derived(!!data && data.applications.length + data.databases.length > 0);

  async function refresh() {
    data = await servicesApi.page(service);
    if (section === 'storages') storages = (await servicesApi.storages(service)).parts;
  }

  async function load() {
    if (!isSection) {
      router.go(base);
      return;
    }
    let d: ServicePage;
    try {
      d = await servicesApi.page(service);
    } catch (e) {
      if (e instanceof ApiError && (e.status === 404 || e.status === 403)) {
        missing = true;
        return;
      }
      throw e;
    }
    data = d;
    document.title = `${d.service.name.slice(0, 10)} > Configuration | coolify-rs`;
    page.navContext = serviceNavContext(base, section, {
      isDeployable: d.is_deployable,
      canAccessTerminal: d.can_access_terminal,
      storageSections:
        section === 'storages'
          ? [...d.applications, ...d.databases].map((p) => ({ id: `storage-service-${p.uuid}`, label: headline(p.name) }))
          : [],
    });
    if (section === 'storages') storages = (await servicesApi.storages(service)).parts;
    uiApi.project(uuid).then((p) => setProjectCrumbs(p, env));
    dbUi
      .resources(uuid, env)
      .then((r) => {
        const list = (r as unknown as { services?: { uuid: string; name: string }[] }).services ?? [];
        resources = list
          .map((s) => ({ label: s.name, href: `/project/${uuid}/environment/${env}/service/${s.uuid}`, active: s.uuid === service }))
          .sort((a, b) => strnatcmp(a.label.toLowerCase(), b.label.toLowerCase()));
      })
      .catch(() => {
        resources = data ? [{ label: data.service.name, href: base, active: true }] : [];
      });
  }

  $effect(() => {
    load();
    // Heading: `wire:poll.10000ms="checkStatus"`.
    const poll = setInterval(() => data && servicesApi.page(service).then((d) => (data = d)).catch(() => {}), 10000);
    return () => clearInterval(poll);
  });

  const refreshQuiet = () => refresh().catch(() => {});
</script>

{#if missing}
  <div class="application-settings-form w-full">
    <Empty title="Service not found" description="It may have been deleted, or it belongs to another team." iconName="layers" size="lg" />
  </div>
{:else if data}
  <div>
    <ServiceHeading {data} {base} {resources} onrefresh={refreshQuiet} />

    <section class="application-settings-workspace mt-4 w-full max-w-none lg:mt-0">
      <div class="grid min-w-0 gap-8 xl:grid-cols-[210px_minmax(0,1fr)] xl:gap-8">
        <aside class="application-settings-navigation in-main-nav min-w-0"></aside>
        <div class="min-w-0">
          {#if section === 'general'}
            {#key data.service.uuid}
              <ServiceStackForm {data} onsaved={refreshQuiet} />
            {/key}

            <div class="mt-8">
              <SettingsSection id="compose-resources-section" title="Compose resources" description="Applications and databases defined in this service." flush>
                {#snippet actions()}
                  {#if hasParts}<ViewToggle bind:mode={viewMode} />{/if}
                  <a class="button" target="_blank" rel="noreferrer" href={data?.documentation}>
                    Documentation
                    <Reicon name="external-link" class="size-4" />
                  </a>
                {/snippet}

                <div class={viewMode === 'grid' ? 'grid grid-cols-1 gap-3 p-3 sm:grid-cols-2' : 'overflow-x-auto rounded-lg'}>
                  {#if hasParts && viewMode === 'table'}
                    <div
                      class="grid min-w-[48rem] grid-cols-[minmax(14rem,1fr)_minmax(12rem,1fr)_12rem_5rem] gap-3 border-b border-neutral-200 bg-neutral-50 px-4 py-2.5 text-[11px] font-medium text-neutral-500 dark:border-white/[0.08] dark:bg-white/[0.05] dark:text-fg-faint"
                    >
                      <div>Resource</div>
                      <div>Image</div>
                      <div class="justify-self-start">Status</div>
                      <div></div>
                    </div>
                  {/if}
                  {#if !hasParts}
                    <div class="p-4 sm:col-span-2">
                      <Empty title="No compose resources" description="No applications or databases are defined in this Docker Compose file." iconName="grid" />
                    </div>
                  {/if}
                  {#each data.applications as resource (resource.id)}
                    <ServiceResourceCard serviceUuid={service} {resource} isDatabase={false} {viewMode} canUpdate={data.can_update} {base} onrefresh={refreshQuiet} />
                  {/each}
                  {#each data.databases as resource (resource.id)}
                    <ServiceResourceCard serviceUuid={service} {resource} isDatabase {viewMode} canUpdate={data.can_update} {base} onrefresh={refreshQuiet} />
                  {/each}
                </div>
              </SettingsSection>
            </div>
          {:else if section === 'domains'}
            <ServiceDomains serviceUuid={service} />
            <div class="mt-6"><Maintenance kind="service" uuid={service} /></div>
          {:else if section === 'storages'}
            <div class="space-y-6">
              <div
                class="rounded-lg border border-amber-200 bg-amber-50 px-4 py-3 text-[13px] leading-5 text-amber-800 dark:border-warning/15 dark:bg-warning/[0.07] dark:text-amber-300/90"
              >
                Service volume mounts are read-only here. Edit the Docker Compose file and reload it to change volumes.
              </div>
              {#each storages ?? [] as part (part.uuid)}
                <ServiceStorage serviceUuid={service} {part} canUpdate={data.can_update} onchanged={refreshQuiet} />
              {/each}
            </div>
          {:else if section === 'environment-variables'}
            <EnvironmentVariables kind="service" uuid={service} onconfigurationchanged={refreshQuiet} />
            <SecretManagerLinks kind="service" uuid={service} />
          {:else if section === 'scheduled-tasks'}
            <ServiceSharedPlaceholder section="Project/Shared/ScheduledTask/All" />
          {:else if section === 'webhooks'}
            <Webhooks kind="service" uuid={service} />
          {:else if section === 'resource-operations'}
            <ResourceOperations kind="service" uuid={service} />
          {:else if section === 'tags'}
            <Tags kind="service" uuid={service} />
          {:else if section === 'danger'}
            <DatabaseDanger kind="service" record={{ uuid: service, name: data.service.name }} canDelete={data.can_update} back="/project/{uuid}/environment/{env}" />
          {:else if section === 'logs'}
            <div class="runtime-logs-column min-w-0"><RuntimeLogs kind="service" uuid={service} /></div>
          {:else if section === 'terminal'}
            <ServiceSharedPlaceholder section="Project/Shared/ExecuteContainerCommand" />
          {:else if section === 'files'}
            <FileBrowser kind="service" uuid={service} />
          {:else}
            <Unported />
          {/if}
        </div>
      </div>
    </section>
  </div>
{/if}
