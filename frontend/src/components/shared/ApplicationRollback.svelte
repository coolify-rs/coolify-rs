<script lang="ts">
  import SkeletonList from '../SkeletonList.svelte';
  // Port of upstream livewire/project/application/rollback.blade.php.
  import Callout from '../Callout.svelte';
  import Empty from '../Empty.svelte';
  import FormInput from '../FormInput.svelte';
  import Reicon from '../Reicon.svelte';
  import SettingsSection from '../SettingsSection.svelte';
  import StatusBadge from '../StatusBadge.svelte';
  import UnsavedBar from '../UnsavedBar.svelte';
  import { ApiError, request } from '../../lib/api';
  import { resPath } from '../../lib/sharedApi';
  import { diffForHumans } from '../../lib/format';
  import { router } from '../../lib/router.svelte';
  import { notify } from '../../lib/toast.svelte';

  let { uuid, base = '' }: { uuid: string; base?: string } = $props();

  let data = $state<any>(null);
  let keep = $state('2');
  let saved = $state('2');
  let saving = $state(false);
  const path = $derived(`${resPath('application', uuid)}/rollback`);

  async function load(toast = false) {
    data = await request<any>('GET', path);
    keep = saved = String(data.docker_images_to_keep);
    if (toast) notify.success('Images loaded.');
  }

  $effect(() => {
    load();
  });

  // `rollbackImage($commit)`.
  async function rollback(commit: string) {
    try {
      const r = await request<{ deployment_uuid?: string; title?: string; message?: string }>('POST', `${path}/start`, { commit });
      if (r.deployment_uuid) router.go(`${base}/deployment/${r.deployment_uuid}`);
      else notify.error(r.title ?? 'Error', r.message);
    } catch (err) {
      notify.failure(err instanceof ApiError ? err.message : String(err));
    }
  }

  async function submit() {
    saving = true;
    try {
      const r = await request<{ message: string }>('POST', path, { dockerImagesToKeep: keep });
      notify.success(r.message);
      saved = keep;
    } catch (err) {
      notify.failure(err instanceof ApiError ? err.message : String(err));
    } finally {
      saving = false;
    }
  }
</script>

{#if data}
  <div class="flex flex-col gap-6">
    <SettingsSection title="Rollback images" description="Keep previously built Docker images available for fast rollbacks. Rollback uses an existing image without rebuilding the application. Images in the Docker registry are pulled to the server first." flush>
      {#snippet actions()}
        <button type="button" class="button" onclick={() => load(true)}>Reload images</button>
      {/snippet}
      <form
        onsubmit={(e) => {
          e.preventDefault();
          submit();
        }}
        class="flex flex-col gap-4 border-b border-neutral-200 px-4 py-3.5 dark:border-white/[0.07]"
      >
        <UnsavedBar dirty={keep !== saved} {saving} onsave={submit} onreset={() => (keep = saved)} />
        {#if data.server_retention_disabled}
          <Callout type="warning" title="Disabled by the server">Image retention is disabled at the server level. This setting has no effect until a server administrator enables it.</Callout>
        {/if}
        <div class="max-w-sm">
          <FormInput id="dockerImagesToKeep" type="number" min="0" max="100" label="Images to keep" helper="Set to 0 to keep only the running image. Pull request images are always removed during cleanup." disabled={!data.can_update || data.server_retention_disabled} bind:value={keep} />
        </div>
      </form>
      <div>
        {#each data.images as image (image.tag)}
          {@const rollbackable = /^[0-9a-f]{7,128}$/i.test(image.tag) || /^pr-\d+$/.test(image.tag)}
          <div class="flex flex-col gap-3 border-b border-neutral-200 px-4 py-3.5 last:border-b-0 sm:flex-row sm:items-center dark:border-white/[0.07]">
            <div class="flex size-9 shrink-0 items-center justify-center rounded-lg bg-neutral-100 text-neutral-500 ring-1 ring-neutral-200 dark:bg-white/[0.05] dark:text-fg-dim dark:ring-white/[0.07]">
              <Reicon name="layers" class="size-[18px]" />
            </div>
            <div class="min-w-0 flex-1">
              <div class="flex flex-wrap items-center gap-2">
                <code class="truncate font-mono text-[13px] font-semibold text-black dark:text-fg">{image.tag}</code>
                {#if image.is_current}
                  <StatusBadge label="Running image" type="success" />
                {:else if !rollbackable}
                  <StatusBadge label="Rollback unavailable" type="neutral" />
                {/if}
              </div>
              <p class="mt-1 text-xs text-neutral-500 dark:text-fg-dim">Built {diffForHumans(String(image.created_at).replace(' ', 'T').replace(/ ([+-]\d{2})(\d{2}).*$/, '$1:$2'))} <span class="mx-1 text-neutral-300 dark:text-fg-faint">·</span> {image.created_at}</p>
            </div>
            {#if data.can_update}
              {#if image.is_current}
                <button type="button" class="button" disabled title="This image is currently running.">Rollback</button>
              {:else if !rollbackable}
                <button type="button" class="button" disabled title="Only commit and pull-request image tags support rollback.">Rollback</button>
              {:else}
                <button type="button" class="button" onclick={() => rollback(image.tag)}>Roll back to this image</button>
              {/if}
            {/if}
          </div>
        {:else}
          <Empty title="No rollback images" description="No previous application images are stored on this server or in the Docker registry." iconName="layers" />
        {/each}
      </div>
    </SettingsSection>
  </div>
{:else}
  <SettingsSection id="rollback-section" title="Rollback images" flush>
    <SkeletonList rows={3} label="Loading available images" />
  </SettingsSection>
{/if}
