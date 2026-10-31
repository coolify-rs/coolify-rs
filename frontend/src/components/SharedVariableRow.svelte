<script lang="ts">
  // Port of upstream livewire/project/shared/environment-variable/show.blade.php (shared variable
  // rows: the modal without the resource-only fields).
  import Reicon from './Reicon.svelte';
  import Modal from './Modal.svelte';
  import FormInput from './FormInput.svelte';
  import FormTextarea from './FormTextarea.svelte';
  import Listbox from './Listbox.svelte';
  import ConfirmModal from './ConfirmModal.svelte';
  import { ApiError } from '../lib/api';
  import { sharedVariablesUi, type SharedVariable } from '../lib/sharedVariablesApi';
  import { notify } from '../lib/toast.svelte';

  let {
    variable,
    scopeLabel,
    canUpdate,
    connectLink,
    order,
    onchange,
  }: { variable: SharedVariable; scopeLabel: string; canUpdate: boolean; connectLink: string; order: number; onchange: () => void } = $props();

  let open = $state(false);
  let loaded = $state(false);
  let key = $state('');
  let comment = $state('');
  let value = $state('');
  let multiline = $state(false);
  let error = $state<ApiError | null>(null);
  let confirming = $state(false);
  let copied = $state(false);
  let initial = $state('');

  const locked = $derived(variable.is_shown_once);
  const hidden = $derived(!canUpdate);
  const canEditValue = $derived(canUpdate && !locked);
  const snapshot = () => JSON.stringify([key, comment, value, multiline]);
  const dirty = $derived(loaded && snapshot() !== initial);

  async function openEditor() {
    open = true;
    loaded = false;
    error = null;
    key = variable.key;
    comment = variable.comment ?? '';
    multiline = variable.is_multiline;
    value = '';
    if (!hidden) {
      const r = await sharedVariablesUi.value(variable.id);
      value = r.value ?? '';
    }
    loaded = true;
    initial = snapshot();
  }

  async function copyValue() {
    const r = await sharedVariablesUi.value(variable.id);
    await navigator.clipboard?.writeText(r.value ?? '').catch(() => {});
    copied = true;
    setTimeout(() => (copied = false), 1500);
  }

  async function submit(e?: Event) {
    e?.preventDefault();
    error = null;
    try {
      const r = await sharedVariablesUi.update(variable.id, { key, comment: comment === '' && variable.comment === null ? null : comment, value: locked ? null : value, is_multiline: multiline });
      notify.success(r.message);
      open = false;
      onchange();
    } catch (err) {
      const failure = err instanceof ApiError ? err : new ApiError(0, String(err));
      if (Object.keys(failure.errors).length) error = failure;
      else notify.failure(failure.message);
    }
  }

  async function lock() {
    try {
      // Upstream leaves the modal open: the page refreshes behind it.
      await sharedVariablesUi.lock(variable.id);
      onchange();
    } catch (err) {
      notify.failure(err instanceof ApiError ? err.message : String(err));
    }
  }

  async function remove() {
    try {
      const r = await sharedVariablesUi.destroy(variable.id);
      notify.success(r.message);
      confirming = false;
      open = false;
      onchange();
    } catch (err) {
      notify.failure(err instanceof ApiError ? err.message : String(err));
    }
  }
</script>

<div class="env-table-item" data-env-key={variable.key} style="order: {order}">
  <div class="data-table-row env-table-grid-shared">
    <div class="flex min-w-0 items-center gap-2">
      {#if locked}
        <svg class="size-3.5 shrink-0 text-neutral-400 dark:text-fg-faint" viewBox="0 0 24 24" xmlns="http://www.w3.org/2000/svg">
          <g fill="none" stroke="currentColor" stroke-linecap="round" stroke-linejoin="round" stroke-width="2">
            <path d="M5 13a2 2 0 0 1 2-2h10a2 2 0 0 1 2 2v6a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2v-6z" />
            <path d="M11 16a1 1 0 1 0 2 0a1 1 0 0 0-2 0m-3-5V7a4 4 0 1 1 8 0v4" />
          </g>
        </svg>
      {/if}
      <button type="button" class="env-key-label min-w-0 truncate text-left font-mono text-[13px] text-black dark:text-fg" title={variable.key} onclick={openEditor}>
        {variable.key}
      </button>
    </div>
    <div class="env-type-desktop text-[13px] text-neutral-500 dark:text-fg-dim">{scopeLabel}</div>
    <div class="min-w-0 truncate text-[13px] text-neutral-500 dark:text-fg-dim" title={variable.comment || undefined}>
      {variable.comment || '-'}
    </div>
    {#if variable.is_multiline}
      <span class="data-table-cell-check">
        <svg xmlns="http://www.w3.org/2000/svg" fill="none" viewBox="0 0 24 24" stroke-width="2" stroke="currentColor" class="size-4">
          <path stroke-linecap="round" stroke-linejoin="round" d="m4.5 12.75 6 6 9-13.5" />
        </svg>
      </span>
    {:else}
      <span class="data-table-cell-dash">-</span>
    {/if}
    <div class="flex items-center gap-0.5 justify-self-end">
      <a href={connectLink} class="icon-button shrink-0" title="Connect {variable.key} to resources" aria-label="Connect {variable.key} to resources">
        <Reicon name="link" class="size-3.5" />
      </a>
      {#if !locked && !hidden}
        <button type="button" title="Copy value" aria-label="Copy value" class="icon-button group shrink-0" onclick={copyValue}>
          <span class="inline-flex transition-transform duration-150 ease-out group-active:scale-75">
            {#if copied}<Reicon name="check" class="size-3.5 text-success" />{:else}<Reicon name="copy" class="size-3.5" />{/if}
          </span>
        </button>
      {/if}
      <button type="button" class="icon-button shrink-0" title="Edit environment variable" aria-label="Edit environment variable" onclick={openEditor}>
        <Reicon name="settings" class="size-3.5" />
      </button>
    </div>
  </div>
</div>

{#if open}
  <Modal title="Edit environment variable" focusFirstField={false} onclose={() => (open = false)}>
    <form onsubmit={submit} class="flex w-full flex-col gap-4">
      <div class="grid items-end gap-4 sm:grid-cols-2">
        <FormInput id="key" label="Name" disabled={!canEditValue} bind:value={key} error={error?.field('key')} />
        <FormInput
          id="comment"
          label="Comment"
          placeholder="Optional note"
          helper="Add a note to document what this environment variable is used for."
          maxlength={256}
          disabled={!canUpdate}
          bind:value={comment}
          error={error?.field('comment')}
        />
      </div>

      <div>
        {#if hidden}
          <div class="w-full">
            <!-- svelte-ignore a11y_label_has_associated_control -->
            <label class="mb-1 flex items-center gap-1 text-sm font-medium">Value</label>
            <input disabled type="text" value="Hidden (only admins can view)" class="input w-full italic !text-neutral-500 dark:!text-neutral-500" />
          </div>
        {:else if locked}
          <div class="w-full">
            <!-- svelte-ignore a11y_label_has_associated_control -->
            <label class="mb-1 flex items-center gap-1 text-sm font-medium">Value</label>
            <input disabled type="text" value="Hidden after locking" class="input w-full italic !text-neutral-500 dark:!text-neutral-500" />
          </div>
        {:else if multiline}
          <div class="w-full">
            <!-- svelte-ignore a11y_label_has_associated_control -->
            <label class="mb-1 flex items-center gap-1 text-sm font-medium">Value</label>
            <FormTextarea id="value" disabled={!canEditValue} bind:value />
          </div>
        {:else}
          <div class="w-full">
            <!-- svelte-ignore a11y_label_has_associated_control -->
            <label class="mb-1 flex items-center gap-1 text-sm font-medium">Value</label>
            <div class="relative">
              <FormInput id="value" type="password" copyable={false} disabled={!canEditValue} bind:value />
            </div>
          </div>
        {/if}
      </div>

      <div class="grid gap-4 border-t border-neutral-200 pt-4 dark:border-white/[0.07] sm:grid-cols-2">
        <Listbox
          id="is_multiline"
          label="Value type"
          disabled={!canUpdate}
          bind:value={multiline}
          options={[
            { value: false, label: 'Single line' },
            { value: true, label: 'Multiline' },
          ]}
        />
      </div>

      {#if canUpdate}
        <div class="flex flex-wrap items-center justify-between gap-2 border-t border-neutral-200 pt-4 dark:border-white/[0.07]">
          <div>
            <button type="button" class="button button-error" onclick={() => (confirming = true)}>Delete</button>
          </div>
          <div class="ml-auto flex flex-wrap gap-2">
            <a href={connectLink} class="button" title="Add this variable to resources, and see which resources use it">
              <Reicon name="link" class="size-3.5" />
              Connect to resources
            </a>
            {#if !locked}
              <button type="button" class="button" onclick={lock}>Lock</button>
            {/if}
            <fieldset disabled={!dirty}>
              <button type="submit" class="button" {...{ isHighlighted: '' }}>Update variable</button>
            </fieldset>
          </div>
        </div>
      {/if}
    </form>
  </Modal>
{/if}

{#if confirming}
  <ConfirmModal
    title="Confirm Environment Variable Deletion?"
    actions={['The selected environment variable will be permanently deleted.']}
    confirmationText={variable.key}
    confirmationLabel="Please confirm the execution of the actions by entering the Environment Variable Name below"
    shortConfirmationLabel="Environment Variable Name"
    step2ButtonText="Permanently Delete"
    onconfirm={remove}
    onclose={() => (confirming = false)}
  />
{/if}
