<script lang="ts">
  // The cloud-init part of the "Advanced options" of `Server\New\By{Hetzner,DigitalOcean,Vultr}`.
  // Vultr always shows the fields; the others start with the "Add" button.
  import Checkbox from './Checkbox.svelte';
  import FormInput from './FormInput.svelte';
  import FormTextarea from './FormTextarea.svelte';
  import Listbox from './Listbox.svelte';
  import Reicon from './Reicon.svelte';
  import type { ApiError } from '../lib/api';
  import { filled, type CloudScript } from '../lib/cloudApi';

  let {
    scripts,
    alwaysOpen = false,
    show = $bindable(false),
    script = $bindable(''),
    selectedId = $bindable(''),
    save = $bindable(false),
    name = $bindable(''),
    error,
  }: {
    scripts: CloudScript[];
    alwaysOpen?: boolean;
    show?: boolean;
    script?: string;
    selectedId?: string;
    save?: boolean;
    name?: string;
    error?: ApiError | null;
  } = $props();

  const options = $derived([{ value: '', label: 'Start with an empty script' }, ...scripts.map((s) => ({ value: String(s.id), label: s.name }))]);

  /** `updatedSelectedCloudInitScriptId`. */
  function pick(id: string) {
    const found = scripts.find((s) => String(s.id) === String(id));
    if (found) {
      script = found.script;
      name = found.name;
      show = true;
    }
  }

  /** `clearCloudInitScript`. */
  function clear() {
    selectedId = '';
    script = '';
    name = '';
    save = false;
    show = false;
  }
</script>

{#snippet fields()}
  <div class="flex flex-col gap-4">
    <div class="grid items-end gap-3 lg:grid-cols-[minmax(0,1fr)_auto]">
      <Listbox id="selected_cloud_init_script_id" label="Saved cloud-init script" bind:value={selectedId} {options} onchange={pick} error={error?.field('selected_cloud_init_script_id')} />
      <button type="button" class="button" onclick={clear}>Clear</button>
    </div>
    <FormTextarea id="cloud_init_script" label="Cloud-init script" rows={8} monospace bind:value={script} error={error?.field('cloud_init_script')} />
    <div class="grid items-end gap-4 lg:grid-cols-2">
      <!-- `updatedSaveCloudInitScript` exists on Hetzner and DigitalOcean only. -->
      <Checkbox id="save_cloud_init_script" label="Save this script for later" bind:checked={save} onchange={(v) => !v && !alwaysOpen && (name = '')} />
      {#if save}
        <FormInput id="cloud_init_script_name" label="Saved script name" bind:value={name} error={error?.field('cloud_init_script_name')} />
      {/if}
    </div>
  </div>
{/snippet}

<div class="border-t border-neutral-200 pt-4 dark:border-white/[0.08]">
  {#if alwaysOpen || show || filled(script) || filled(selectedId)}
    {@render fields()}
  {:else}
    <button type="button" class="button" onclick={() => (show = true)}>
      <Reicon name="plus" class="size-3.5" />
      Add cloud-init script
    </button>
  {/if}
</div>
