<script lang="ts">
  // Port of upstream livewire/destination/show.blade.php, resources.blade.php and sidebar.blade.php.
  import Reicon from '../components/Reicon.svelte';
  import Empty from '../components/Empty.svelte';
  import Callout from '../components/Callout.svelte';
  import ConfirmModal from '../components/ConfirmModal.svelte';
  import DangerZone from '../components/DangerZone.svelte';
  import FormInput from '../components/FormInput.svelte';
  import SettingsSection from '../components/SettingsSection.svelte';
  import StatusBadge from '../components/StatusBadge.svelte';
  import UnsavedBar from '../components/UnsavedBar.svelte';
  import { ApiError } from '../lib/api';
  import { get, patch, post, failure, type DestinationData, type DestinationListData, type DestinationResourceRow } from '../lib/serverApi';
  import { page } from '../lib/page.svelte';
  import { link, router } from '../lib/router.svelte';
  import { notify } from '../lib/toast.svelte';

  let { uuid, section }: { uuid: string; section: string } = $props();

  let data = $state<DestinationData | null>(null);
  let missing = $state(false);
  let name = $state('');
  let saved = $state('');
  let saving = $state(false);
  let error = $state<ApiError | null>(null);
  let rows = $state<DestinationResourceRow[]>([]);
  let search = $state('');
  let deleting = $state(false);

  const standalone = $derived(!!data && !data.is_swarm);
  const subtitle = $derived(
    section === 'resources'
      ? 'Applications, databases, and services on this network'
      : standalone
        ? `Docker network on ${data?.server_name ?? 'server'}`
        : 'Deprecated Docker Swarm network',
  );

  get<DestinationData>(`/destination-list/${encodeURIComponent(uuid)}`)
    .then(async (d) => {
      // `Destination\Resources::mount`: Swarm networks have no resources page.
      if (section === 'resources' && d.is_swarm) {
        router.go(`/destination/${d.uuid}`, true);
        return;
      }
      data = d;
      name = saved = d.name;
      document.title = section === 'resources' ? `${d.name} Resources | coolify-rs` : `${d.name} | Destination | coolify-rs`;
      const base = `/destination/${d.uuid}`;
      page.navContext = {
        label: 'Destinations',
        backTitle: 'Back to Integrations',
        ariaLabel: 'Destination settings',
        name: d.name,
        groups: [
          [
            { label: 'General', icon: 'settings', href: base, active: section === 'general' },
            ...(d.is_swarm ? [] : [{ label: 'Resources', icon: 'grid', href: `${base}/resources`, active: section === 'resources' }]),
          ],
          [{ label: 'Danger Zone', icon: 'danger', href: `${base}/danger`, active: section === 'danger' }],
        ],
      };
      get<DestinationListData>('/destination-list').then((l) => {
        // The switcher lists destinations of usable servers.
        if (!l.destinations.some((x) => x.uuid === d.uuid)) return;
        page.switchers = [
          {
            heading: 'Destinations',
            title: 'Destinations',
            label: d.name,
            uuid: d.uuid,
            meta: { label: d.is_swarm ? 'Deprecated' : 'Docker', dot: d.is_swarm ? 'bg-warning' : 'bg-[#3fb950]' },
            items: [...l.destinations].sort((a, b) => a.name.localeCompare(b.name)).map((x) => ({ label: x.name, uuid: x.uuid, href: `/destination/${x.uuid}`, active: x.uuid === d.uuid })),
          },
        ];
      });
      if (section === 'resources') rows = (await get<{ resources: DestinationResourceRow[] }>(`/destination-list/${encodeURIComponent(uuid)}/resources`)).resources;
    })
    .catch((e) => {
      if (e instanceof ApiError && e.status === 404) router.go('/destinations', true);
      else missing = true;
    });

  async function save() {
    saving = true;
    error = null;
    try {
      await patch(`/destination-list/${encodeURIComponent(uuid)}`, { name });
      saved = name;
      page.nameChanged(uuid, name);
      notify.success('Destination saved.');
    } catch (e) {
      error = failure(e);
    } finally {
      saving = false;
    }
  }

  async function remove(): Promise<void> {
    try {
      await post(`/destination-list/${encodeURIComponent(uuid)}/delete`);
      router.go('/destinations');
    } catch (e) {
      deleting = false;
      failure(e);
    }
  }

  const typeLabel = (t: string) => t.charAt(0).toUpperCase() + t.slice(1);
  const rowClass =
    'grid min-h-13 min-w-[680px] grid-cols-[minmax(10rem,.8fr)_minmax(10rem,.8fr)_minmax(12rem,1fr)_8rem] items-center border-b border-neutral-200 px-4 py-2.5 text-[12px] last:border-b-0 dark:border-white/[0.07]';
</script>

{#if missing}
  <div class="application-settings-form w-full">
    <Empty title="Destination not found" description="It may have been deleted, or it belongs to another team." iconName="destinations" size="lg" />
  </div>
{:else if data}
  <div>
    <nav class="w-full max-w-none pb-3 lg:pb-0">
      <div class="mb-3 flex w-full min-w-0 items-start gap-3 lg:hidden">
        <div class="min-w-0 flex-1">
          <h1 class="min-w-0 truncate text-[24px]! leading-7! font-semibold! tracking-tight! text-black dark:text-fg">{section === 'resources' ? data.name : name}</h1>
          <p class="sr-only">{subtitle}</p>
        </div>
      </div>
    </nav>
    <section class="application-settings-workspace mt-4 w-full max-w-none lg:mt-0">
      <div class="grid min-w-0 gap-8 xl:grid-cols-[210px_minmax(0,1fr)] xl:gap-8">
        <aside class="application-settings-navigation in-main-nav min-w-0"></aside>
        <div class="min-w-0">
          {#if section === 'resources'}
            <div class="application-settings-form">
              <SettingsSection title="Resources" description="Applications, databases, and services connected to this Docker network." flush>
                {#if rows.length === 0}
                  <Empty title="No resources use this destination" description="Resources will appear here after they are deployed to this network." iconName="destinations" size="sm" />
                {:else}
                  <div class="border-b border-neutral-200 p-3 dark:border-white/[0.08]">
                    <div class="relative w-full max-w-sm">
                      <Reicon name="search" class="pointer-events-none absolute top-1/2 left-2.5 z-10 size-3.5 -translate-y-1/2 text-neutral-400 dark:text-fg-faint" />
                      <input
                        bind:value={search}
                        type="search"
                        placeholder="Search resources"
                        class="h-8! w-full rounded-lg! border-neutral-200! bg-white! py-0! pr-3! pl-8! text-[12px]! shadow-none! placeholder:text-neutral-400 focus:border-accent! focus:ring-0! dark:border-white/[0.08]! dark:bg-white/[0.035]! dark:text-fg! dark:placeholder:text-fg-faint"
                      />
                    </div>
                  </div>
                  <div class="overflow-x-auto">
                    <div class="grid min-w-[680px] grid-cols-[minmax(10rem,.8fr)_minmax(10rem,.8fr)_minmax(12rem,1fr)_8rem] border-b border-neutral-200 bg-neutral-50 px-4 py-2.5 text-[11px] font-medium text-neutral-500 dark:border-white/[0.08] dark:bg-white/[0.05] dark:text-fg-faint">
                      <div>Project</div>
                      <div>Environment</div>
                      <div>Resource</div>
                      <div>Type</div>
                    </div>
                    {#each rows.filter((r) => search === '' || r.search.includes(search.toLowerCase())) as row (row.type + row.uuid)}
                      {#if row.url}
                        <a href={row.url} use:link class="{rowClass} transition-colors hover:bg-neutral-50 hover:no-underline dark:hover:bg-white/[0.025]">
                          <span class="truncate text-neutral-500 dark:text-fg-dim">{row.project}</span>
                          <span class="truncate text-neutral-500 dark:text-fg-dim">{row.environment}</span>
                          <span class="truncate font-medium text-black dark:text-fg">{row.name}</span>
                          <span class="text-neutral-500 dark:text-fg-dim">{typeLabel(row.type)}</span>
                        </a>
                      {:else}
                        <div class={rowClass}>
                          <span class="truncate text-neutral-500 dark:text-fg-dim">{row.project}</span>
                          <span class="truncate text-neutral-500 dark:text-fg-dim">{row.environment}</span>
                          <span class="truncate font-medium text-black dark:text-fg">{row.name}</span>
                          <span class="text-neutral-500 dark:text-fg-dim">{typeLabel(row.type)}</span>
                        </div>
                      {/if}
                    {/each}
                  </div>
                {/if}
              </SettingsSection>
            </div>
          {:else}
            {#if data.is_swarm}
              <Callout type="warning" title="Docker Swarm support is deprecated" class="mb-6">{data.swarm_notice}</Callout>
            {/if}
            {#if section === 'danger'}
              <div class="application-settings-form">
                <SettingsSection id="destination-danger-section" title="Danger zone" description="Destructive actions for this destination cannot be undone.">
                  <DangerZone title="Delete destination">
                    <p>Permanently delete <strong class="font-semibold">{data.name}</strong> from coolify-rs. The Docker network is also removed from the server.</p>
                    <p>Delete or move every attached resource before deleting this destination.</p>
                    {#snippet action()}
                      {#if data?.network !== 'coolify'}
                        <button type="button" class="button button-error flex gap-2" disabled={!data?.can_update} onclick={() => (deleting = true)}>Delete destination</button>
                      {:else}
                        <button type="button" class="button button-error" disabled title="The default coolify-rs destination cannot be deleted.">Delete destination</button>
                      {/if}
                    {/snippet}
                  </DangerZone>
                </SettingsSection>
              </div>
            {:else}
              <form
                class="application-settings-form"
                onsubmit={(e) => {
                  e.preventDefault();
                  save();
                }}
              >
                <UnsavedBar dirty={name !== saved} {saving} onsave={save} onreset={() => ((name = saved), (error = null))} />
                <SettingsSection title="General" description={standalone ? 'Docker network used to connect deployed resources.' : 'Deprecated Docker Swarm network.'}>
                  {#snippet actions()}
                    {#if !standalone}
                      <StatusBadge label="Deprecated" type="warning" />
                    {/if}
                  {/snippet}
                  <div class="grid gap-4 lg:grid-cols-2">
                    <FormInput id="name" label="Name" disabled={!data.can_update} bind:value={name} error={error?.field('name')} />
                    <FormInput id="serverIp" label="Server IP" readonly value={data.server_ip ?? ''} />
                    {#if standalone}
                      <div class="lg:col-span-2">
                        <FormInput id="network" label="Docker network" readonly value={data.network} />
                      </div>
                    {/if}
                  </div>
                </SettingsSection>
              </form>
            {/if}
          {/if}
        </div>
      </div>
    </section>
  </div>
{/if}

{#if deleting && data}
  <ConfirmModal
    title="Confirm Destination Deletion?"
    actions={['This permanently deletes the destination and its Docker network.']}
    confirmationText={data.name}
    confirmationLabel="Please confirm by entering the Destination Name below"
    shortConfirmationLabel="Destination Name"
    step2ButtonText="Permanently Delete"
    onconfirm={remove}
    onclose={() => (deleting = false)}
  />
{/if}
