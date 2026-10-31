<script lang="ts">
  // The private key field of `Server\New\By*`: the listbox, or the "required" box
  // with its `x-modal-input` when the team has no SSH key.
  import AddPrivateKeyModal from './AddPrivateKeyModal.svelte';
  import Listbox from './Listbox.svelte';
  import { type CloudKey } from '../lib/cloudApi';

  let {
    keys,
    value = $bindable(''),
    helper,
    error,
    refresh,
  }: { keys: CloudKey[]; value?: string; helper: string; error?: string; refresh: () => Promise<void> } = $props();

  let adding = $state(false);

  async function created() {
    // `handlePrivateKeyCreated`: reloads the keys and selects the new one.
    adding = false;
    const before = new Set(keys.map((k) => k.id));
    await refresh();
    const added = keys.find((k) => !before.has(k.id));
    if (added) value = String(added.id);
  }
</script>

{#if keys.length === 0}
  <div>
    <label class="mb-1.5 flex w-fit items-center gap-1.5">Private key
      <span class="text-helper">*</span>
    </label>
    <div class="flex min-h-8 items-center justify-between gap-3 rounded-lg border border-warning/30 bg-warning/5 px-3 py-2">
      <span class="text-[11px] text-neutral-600 dark:text-fg-dim">A private key is required.</span>
      <button type="button" class="button" onclick={() => (adding = true)}>Create key</button>
    </div>
  </div>
{:else}
  <Listbox id="private_key_id" label="Private key" required placeholder="Select a private key" {helper} {error} bind:value options={keys.map((k) => ({ value: String(k.id), label: k.name }))} />
{/if}

{#if adding}
  <AddPrivateKeyModal title="New Private Key" onclose={() => (adding = false)} oncreated={created} />
{/if}
