<script lang="ts">
  // Port of upstream livewire/project/shared/tags.blade.php.
  import Callout from '../Callout.svelte';
  import Empty from '../Empty.svelte';
  import FormInput from '../FormInput.svelte';
  import Reicon from '../Reicon.svelte';
  import SettingsSection from '../SettingsSection.svelte';
  import { ApiError, request } from '../../lib/api';
  import { resPath, type ResourceKind } from '../../lib/sharedApi';
  import { notify } from '../../lib/toast.svelte';

  let { kind, uuid }: { kind: ResourceKind; uuid: string } = $props();

  interface Tag {
    id: number;
    name: string;
  }
  let data = $state<{ can_update: boolean; assigned: Tag[]; available: Tag[] } | null>(null);
  let newTags = $state('');

  const base = $derived(`${resPath(kind, uuid)}/tags`);

  async function load() {
    data = await request('GET', base);
  }

  $effect(() => {
    load();
  });

  function failure(err: unknown) {
    notify.failure(err instanceof ApiError ? err.message : String(err));
  }

  async function submit(e: Event) {
    e.preventDefault();
    try {
      const r = await request<{ toasts: { title: string; message: string }[] }>('POST', base, { newTags });
      r.toasts.forEach((t) => notify.error(t.title, t.message));
      newTags = '';
      await load();
    } catch (err) {
      failure(err);
    }
  }

  async function add(id: number) {
    try {
      const r = await request<{ message: string }>('POST', `${base}/${id}`, {});
      notify.success(r.message);
      await load();
    } catch (err) {
      failure(err);
    }
  }

  async function remove(id: number) {
    try {
      const r = await request<{ message: string }>('DELETE', `${base}/${id}`);
      notify.success(r.message);
      await load();
    } catch (err) {
      failure(err);
    }
  }
</script>

{#if data}
  <div class="flex flex-col gap-6">
    <SettingsSection id="tag-assignment-section" title="Tags" description="Organize this resource with reusable team tags. Separate multiple tag names with spaces.">
      {#if data.can_update}
        <form onsubmit={submit} class="grid gap-4 md:grid-cols-[minmax(0,1fr)_auto] md:items-end">
          <FormInput id="newTags" label="Tag names" helper="Existing tags are assigned automatically. New names create team tags." placeholder="production api customer-a" bind:value={newTags} />
          <button type="submit" class="button button-highlighted">
            <Reicon name="plus" class="size-3.5" />
            Add tags
          </button>
        </form>
      {:else}
        <Callout type="danger" title="Insufficient permissions">You do not have permission to manage tags for this resource.</Callout>
      {/if}
    </SettingsSection>

    <SettingsSection id="assigned-tags-section" title="Assigned tags" description="Tags currently attached to this resource." flush>
      {#each data.assigned as tag (tag.id)}
        <div class="flex min-h-12 items-center gap-3 border-b border-neutral-200 px-4 py-2.5 last:border-b-0 dark:border-white/[0.07]">
          <div class="flex size-8 shrink-0 items-center justify-center rounded-lg bg-neutral-100 text-neutral-500 ring-1 ring-neutral-200 dark:bg-white/[0.05] dark:text-fg-dim dark:ring-white/[0.07]">
            <Reicon name="tags" class="size-4" />
          </div>
          <span class="min-w-0 flex-1 truncate text-sm font-medium text-black dark:text-fg">{tag.name}</span>
          {#if data.can_update}
            <button type="button" class="button button-error h-7!" onclick={() => remove(tag.id)}>Remove</button>
          {/if}
        </div>
      {:else}
        <Empty size="sm" title="No tags assigned" description="Add a tag above or select one from your team's available tags." iconName="tags" />
      {/each}
    </SettingsSection>

    {#if data.can_update && data.available.length > 0}
      <SettingsSection id="available-tags-section" title="Available tags" description="Assign an existing team tag with one click.">
        <div class="flex flex-wrap gap-2">
          {#each data.available as tag (tag.id)}
            <button type="button" class="button" onclick={() => add(tag.id)}>
              <Reicon name="plus" class="size-3.5 text-coollabs dark:text-brand-light" />
              {tag.name}
            </button>
          {/each}
        </div>
      </SettingsSection>
    {/if}
  </div>
{/if}
