<script lang="ts">
  // Port of upstream livewire/project/shared/environment-variable/show-hardcoded.blade.php.
  import Reicon from '../Reicon.svelte';
  import Modal from '../Modal.svelte';
  import FormInput from '../FormInput.svelte';
  import Callout from '../Callout.svelte';
  import Helper from '../Helper.svelte';
  import { envsUi, type HardcodedEnvRow, type ResourceKind } from '../../lib/sharedApi';

  let {
    kind,
    uuid,
    row,
    onfocuskey,
  }: { kind: ResourceKind; uuid: string; row: HardcodedEnvRow; onfocuskey: (key: string) => void } = $props();

  let open = $state(false);
  let copied = $state(false);

  async function copyValue() {
    const r = await envsUi(kind, uuid).resolve(row.value, row.scope === 'preview');
    await navigator.clipboard?.writeText(r.value ?? '').catch(() => {});
    copied = true;
    setTimeout(() => (copied = false), 1500);
  }
</script>

<div class="env-table-item">
  <div class="data-table-row env-table-grid env-table-grid-no-type">
    <div class="flex min-w-0 items-center gap-2">
      <button type="button" data-env-name-trigger class="env-key-label min-w-0 truncate text-left font-mono text-[13px] text-black dark:text-fg" title={row.key} onclick={() => (open = true)}>
        {row.key}
      </button>
      {#if row.comment}<Helper helper={row.comment} />{/if}
      {#if row.service_name}<span class="table-badge shrink-0">{row.service_name}</span>{/if}
      <span class="table-badge shrink-0">{row.compose_type === 'derived' ? 'Derived in Compose' : 'Defined in Compose'}</span>
    </div>
    <span class="env-managed-desktop data-table-cell-check">
      <svg xmlns="http://www.w3.org/2000/svg" fill="none" viewBox="0 0 24 24" stroke-width="2" stroke="currentColor" class="size-4">
        <path stroke-linecap="round" stroke-linejoin="round" d="m4.5 12.75 6 6 9-13.5" />
      </svg>
    </span>
    <span class="data-table-cell-dash">-</span>
    <span class="data-table-cell-dash">-</span>
    <span class="data-table-cell-dash">-</span>
    <span class="data-table-cell-dash">-</span>
    <div class="flex items-center justify-end gap-0.5 justify-self-end">
      {#if !row.is_value_hidden}
        <button type="button" title="Copy value" aria-label="Copy value" class="icon-button group shrink-0" onclick={copyValue}>
          <span class="inline-flex transition-transform duration-150 ease-out group-active:scale-75">
            {#if copied}<Reicon name="check" class="size-3.5 text-success" />{:else}<Reicon name="copy" class="size-3.5" />{/if}
          </span>
        </button>
      {/if}
      <div class="relative h-auto w-auto">
      <button type="button" data-env-settings-trigger class="icon-button shrink-0" title="View environment variable" aria-label="View environment variable" onclick={() => (open = true)}>
        <Reicon name="settings" class="size-3.5" />
      </button>
      </div>
    </div>
  </div>
</div>

{#if open}
  <Modal title="Environment variable details" onclose={() => (open = false)}>
    <div class="flex w-full flex-col gap-4">
      <FormInput id="hardcoded-key" label="Name" value={row.key} readonly />
      {#if row.is_value_hidden}
        <div class="w-full">
          <!-- svelte-ignore a11y_label_has_associated_control -->
          <label class="mb-1 flex items-center gap-1 text-sm font-medium">Value</label>
          <input disabled type="text" value="Hidden (only admins can view)" class="input w-full italic !text-neutral-500 dark:!text-neutral-500" />
        </div>
      {:else}
        <FormInput id="hardcoded-value" label="Value" value={row.value ?? ''} readonly />
      {/if}
      {#if row.comment}
        <FormInput id="hardcoded-comment" label="Comment" value={row.comment} readonly />
      {/if}
      <Callout type="info" title={row.compose_type === 'derived' ? 'Derived in Compose' : 'Defined in Compose'}>
        {#if row.compose_type === 'derived'}
          This value is derived from
          {#each row.references as reference, i (reference)}
            <button
              type="button"
              data-compose-reference
              class="font-mono underline underline-offset-2"
              onclick={() => {
                open = false;
                onfocuskey(reference);
              }}>{reference}</button
            >{i === row.references.length - 1 ? '' : ', '}
          {/each}. Edit those variables or update the Compose file.
        {:else}
          This value is defined directly in Docker Compose. Update it in the Compose file.
        {/if}
      </Callout>
    </div>
  </Modal>
{/if}
