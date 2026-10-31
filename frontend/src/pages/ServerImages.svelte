<script lang="ts">
  // Port of upstream livewire/server/docker-images.blade.php.
  import Reicon from '../components/Reicon.svelte';
  import ConfirmModal from '../components/ConfirmModal.svelte';
  import Empty from '../components/Empty.svelte';
  import SkeletonTable from '../components/SkeletonTable.svelte';
  import Pager from '../components/Pager.svelte';
  import ServerFrame from '../components/ServerFrame.svelte';
  import SettingsSection from '../components/SettingsSection.svelte';
  import StatusBadge from '../components/StatusBadge.svelte';
  import { get, post, failure } from '../lib/serverApi';
  import { session } from '../lib/session.svelte';
  import { notify } from '../lib/toast.svelte';

  let { uuid }: { uuid: string } = $props();
  type Image = { id: string; short_id: string; repository: string; tag: string; reference: string; size: string; created: string; containers: string[] };
  let images = $state<Image[]>([]);
  let usage = $state<{ size: string; reclaimable: string } | null>(null);
  let loaded = $state(false);
  let search = $state('');
  let page = $state(1);
  let pageSize = $state(10);
  let deleting = $state<string | null>(null);
  const base = $derived(`/server-page/${encodeURIComponent(uuid)}/images`);

  async function load() {
    try {
      const d = await get<{ images: Image[]; usage: { size: string; reclaimable: string } | null }>(base);
      images = d.images;
      usage = d.usage;
    } catch (e) {
      failure(e);
    } finally {
      loaded = true;
    }
  }
  async function remove() {
    if (!deleting) return;
    const reference = deleting;
    const image = images.find((i) => i.reference === reference);
    if (image && image.containers.length) {
      notify.failure(`This image is used by ${image.containers.join(', ')}. Remove those containers first.`);
      return;
    }
    try {
      const r = await post<{ message: string }>(`${base}/delete`, { reference });
      notify.success(r.message);
      await load();
    } catch (e) {
      failure(e);
    }
  }
  const q = $derived(search.trim().toLowerCase());
  const visible = $derived(images.filter((i) => !q || `${i.repository}:${i.tag} ${i.short_id}`.toLowerCase().includes(q)));
  $effect(() => {
    const last = Math.max(1, Math.ceil(visible.length / pageSize));
    if (page > last) page = last;
  });
</script>

<ServerFrame {uuid} active="images" title="Images" onloaded={load}>
  {#snippet children()}
    <div class="application-settings-form w-full min-w-0">
      <SettingsSection id="server-docker-images-section" title="Images" description="Docker images stored on this server. Images that no container uses can be deleted here." flush>
        {#snippet badges()}
          {#if usage}
            <StatusBadge label="Total {usage.size}" type="neutral" />
            <StatusBadge label="Reclaimable {usage.reclaimable}" type="warning" />
          {/if}
        {/snippet}
        {#snippet actions()}
          <button type="button" class="button" onclick={load}>
            <Reicon name="refresh" class="size-3.5" />
            Refresh
          </button>
        {/snippet}
        <div class="border-b border-neutral-200 p-3 dark:border-white/[0.08]">
          <div class="relative w-full max-w-sm">
            <Reicon name="search" class="pointer-events-none absolute top-1/2 left-2.5 z-10 size-3.5 -translate-y-1/2 text-neutral-400 dark:text-fg-faint" />
            <input
              bind:value={search}
              oninput={() => (page = 1)}
              type="search"
              placeholder="Search images by repository, tag or ID"
              aria-label="Search images"
              class="h-8! w-full rounded-lg! border-neutral-200! bg-white! py-0! pr-8! pl-8! text-[12px]! shadow-none! placeholder:text-neutral-400 focus:border-accent! focus:ring-0! dark:border-white/[0.08]! dark:bg-white/[0.035]! dark:text-fg! dark:placeholder:text-fg-faint"
            />
          </div>
        </div>
        {#if !loaded}
          <div role="status">
            <span class="sr-only">Reading images from the Docker daemon</span>
            <SkeletonTable flush rows={6} />
          </div>
        {:else if visible.length === 0}
          <div class="p-6">
            <Empty size="sm" title={q ? 'No matching images' : 'No images found'} description={q ? 'Try another repository, tag or ID, or clear the search.' : 'Images pulled or built on this server will appear here.'} iconName="layers" />
          </div>
        {:else}
          <div class="data-table">
            <div class="data-table-header server-images-table-grid">
              <span>Repository</span>
              <span>Tag</span>
              <span>Image ID</span>
              <span>Created</span>
              <span>Size</span>
              <span>Used by</span>
              <span>Actions</span>
            </div>
            {#each visible.slice((page - 1) * pageSize, page * pageSize) as image (image.reference)}
              <div class="data-table-row server-images-table-grid border-b border-neutral-200 last:border-b-0 dark:border-white/[0.08]">
                <div class="min-w-0 truncate text-[12px] font-medium text-neutral-950 dark:text-fg" title={image.repository}>{image.repository}</div>
                <div class="min-w-0 truncate text-[11px] text-neutral-600 dark:text-fg-dim">{image.tag}</div>
                <div class="flex min-w-0 items-center gap-1">
                  <span class="truncate font-mono text-[11px] text-neutral-600 dark:text-fg-dim">{image.short_id}</span>
                  <button type="button" aria-label="Copy image ID" title="Copy image ID" class="shrink-0 text-neutral-400 hover:text-black dark:hover:text-fg" onclick={() => navigator.clipboard.writeText(image.id)}>
                    <Reicon name="copy" class="size-3" />
                  </button>
                </div>
                <div class="truncate text-[11px] text-neutral-600 dark:text-fg-dim">{image.created}</div>
                <div class="text-[11px] text-neutral-600 dark:text-fg-dim">{image.size}</div>
                <div class="min-w-0">
                  {#if image.containers.length === 0}
                    <StatusBadge label="Unused" type="warning" />
                  {:else}
                    <span class="block truncate text-[11px] text-neutral-600 dark:text-fg-dim" title={image.containers.join(', ')}>{image.containers.join(', ')}</span>
                  {/if}
                </div>
                <div>
                  {#if image.containers.length === 0 && session.isAdmin}
                    <button type="button" class="button button-error" onclick={() => (deleting = image.reference)}>Delete</button>
                  {/if}
                </div>
              </div>
            {/each}
          </div>
          <Pager total={visible.length} bind:page bind:pageSize storageKey="coolify.page-size.server-docker-images" separator="–" />
        {/if}
      </SettingsSection>
    </div>
  {/snippet}
</ServerFrame>

{#if deleting}
  <ConfirmModal
    title="Delete Docker Image?"
    actions={[`Delete ${deleting} from this server.`, 'It has to be pulled or built again before it can be used.']}
    confirmationText={deleting}
    confirmationLabel="Confirm by entering the image reference"
    shortConfirmationLabel="Image"
    step2ButtonText="Delete Image"
    onconfirm={remove}
    onclose={() => (deleting = null)}
  />
{/if}
