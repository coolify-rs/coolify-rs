<script lang="ts">
  // Port of upstream livewire/project/service/configuration.blade.php (+
  // Project\Service\Configuration) and of the page form of
  // livewire/project/service/index.blade.php. `Project/Shared/*` sections are
  // placeholders until the shared components are connected.
  import Empty from '../components/Empty.svelte';
  import Reicon from '../components/Reicon.svelte';
  import ServiceHeading from '../components/ServiceHeading.svelte';
  import ServiceResourceCard from '../components/ServiceResourceCard.svelte';
  import ServiceResourceForm from '../components/ServiceResourceForm.svelte';
  import ServiceSharedPlaceholder from '../components/ServiceSharedPlaceholder.svelte';
  import ServiceStackForm from '../components/ServiceStackForm.svelte';
  import ServiceStorage from '../components/ServiceStorage.svelte';
  import SettingsSection from '../components/SettingsSection.svelte';
  import ViewToggle from '../components/ViewToggle.svelte';
  import Unported from './Unported.svelte';
  import { ApiError, uiApi, dbUi } from '../lib/api';
  import { headline, servicesApi, type ServicePage, type ServicePart, type ServiceResourceDetails, type ServiceStoragePart } from '../lib/servicesApi';
  import { SERVICE_SECTIONS, serviceNavContext, serviceResourceNavContext } from '../lib/serviceMenu';
  import { setProjectCrumbs } from '../lib/crumbs';
  import { strnatcmp } from '../lib/natsort';
  import { page } from '../lib/page.svelte';
  import { link } from '../lib/router.svelte';

  let { uuid, env, service, section }: { uuid: string; env: string; service: string; section: string } = $props();

  const base = $derived(`/project/${uuid}/environment/${env}/service/${service}`);
  const isSection = $derived(section === 'general' || SERVICE_SECTIONS.includes(section));

  let data = $state<ServicePage | null>(null);
  let details = $state<{ resource: ServiceResourceDetails; can_update: boolean } | null>(null);
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

  const selected = $derived<ServicePart | null>(
    data && !isSection ? ([...data.applications, ...data.databases].find((p) => p.uuid === section) ?? null) : null,
  );
  const hasParts = $derived(!!data && data.applications.length + data.databases.length > 0);

  async function refresh() {
    data = await servicesApi.page(service);
    if (section === 'storages') storages = (await servicesApi.storages(service)).parts;
    if (!isSection) details = await servicesApi.resource(service, section);
  }

  async function load() {
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
    if (isSection) {
      document.title = `${d.service.name.slice(0, 10)} > Configuration | coolify-rs`;
      page.navContext = serviceNavContext(base, section, {
        isDeployable: d.is_deployable,
        canAccessTerminal: d.can_access_terminal,
        storageSections:
          section === 'storages'
            ? [...d.applications, ...d.databases].map((p) => ({ id: `storage-service-${p.uuid}`, label: headline(p.name) }))
            : [],
      });
    } else {
      const part = [...d.applications, ...d.databases].find((p) => p.uuid === section);
      if (!part) {
        missing = true;
        return;
      }
      document.title = `${d.service.name.slice(0, 10)} > ${part.name.slice(0, 10)} | coolify-rs`;
      page.navContext = serviceResourceNavContext(`${base}/${section}`);
      details = await servicesApi.resource(service, section);
    }
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
    <ServiceHeading {data} {base} {resources} {selected} onrefresh={refreshQuiet} />

    <section class="application-settings-workspace mt-4 w-full max-w-none lg:mt-0">
      <div class="grid min-w-0 gap-8 xl:grid-cols-[210px_minmax(0,1fr)] xl:gap-8">
        <aside class="application-settings-navigation in-main-nav min-w-0"></aside>
        <div class="min-w-0">
          {#if !isSection}
            <a class="sr-only" use:link href={base}>Back to service</a>
            {#if details}
              {#key details.resource.uuid}
                <ServiceResourceForm serviceUuid={service} resource={details.resource} canUpdate={details.can_update} serviceBase={base} onsaved={refreshQuiet} />
              {/key}
            {/if}
          {:else if section === 'general'}
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
            <ServiceSharedPlaceholder section="Project/Shared/EnvironmentVariable/All" />
          {:else if section === 'scheduled-tasks'}
            <ServiceSharedPlaceholder section="Project/Shared/ScheduledTask/All" />
          {:else if section === 'webhooks'}
            <ServiceSharedPlaceholder section="Project/Shared/Webhooks" />
          {:else if section === 'resource-operations'}
            <ServiceSharedPlaceholder section="Project/Shared/ResourceOperations" />
          {:else if section === 'tags'}
            <ServiceSharedPlaceholder section="Project/Shared/Tags" />
          {:else if section === 'danger'}
            <ServiceSharedPlaceholder section="Project/Shared/Danger" />
          {:else if section === 'logs'}
            <ServiceSharedPlaceholder section="Project/Shared/Logs" />
          {:else if section === 'terminal'}
            <ServiceSharedPlaceholder section="Project/Shared/ExecuteContainerCommand" />
          {:else if section === 'files'}
            <ServiceSharedPlaceholder section="Project/Shared/FileBrowser" />
          {:else}
            <Unported />
          {/if}
        </div>
      </div>
    </section>
  </div>
{/if}
