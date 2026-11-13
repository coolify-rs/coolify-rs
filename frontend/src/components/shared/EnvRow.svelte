<script lang="ts">
  // Port of upstream livewire/project/shared/environment-variable/show.blade.php for a variable
  // of a resource (shared variables have their own row, SharedVariableRow.svelte).
  import Reicon from '../Reicon.svelte';
  import Modal from '../Modal.svelte';
  import FormInput from '../FormInput.svelte';
  import FormTextarea from '../FormTextarea.svelte';
  import Listbox from '../Listbox.svelte';
  import ConfirmModal from '../ConfirmModal.svelte';
  import Callout from '../Callout.svelte';
  import Helper from '../Helper.svelte';
  import EnvVarInput from './EnvVarInput.svelte';
  import { ApiError } from '../../lib/api';
  import { envsUi, type AvailableVars, type ManagedEnvRow, type ResourceKind } from '../../lib/sharedApi';
  import { buildWarning } from '../../lib/envWarnings';
  import { notify } from '../../lib/toast.svelte';

  let {
    kind,
    uuid,
    row,
    canUpdate,
    availableVars,
    openKey = null,
    onchange,
  }: {
    kind: ResourceKind;
    uuid: string;
    row: ManagedEnvRow;
    canUpdate: boolean;
    availableVars: AvailableVars;
    /** Set by a `focus-compose-environment-variable` link from a hardcoded row. */
    openKey?: string | null;
    onchange: () => void;
  } = $props();

  const api = $derived(envsUi(kind, uuid));
  let open = $state(false);
  let loaded = $state(false);
  let key = $state('');
  let comment = $state('');
  let value = $state('');
  let realValue = $state<string | null>(null);
  let isShared = $state(false);
  let legacy = $state(false);
  let isMultiline = $state(false);
  let isLiteral = $state(false);
  let isBuildtime = $state(true);
  let isRuntime = $state(true);
  let reallyRequired = $state(false);
  let error = $state<ApiError | null>(null);
  let confirming = $state(false);
  let confirmingLock = $state(false);
  /** A new value for a locked variable. The locked value is never sent to the browser, but it can be replaced. */
  let newLockedValue = $state('');
  let copied = $state(false);
  let initial = $state('');
  let root: HTMLDivElement | undefined = $state();

  const locked = $derived(row.is_shown_once);
  const hidden = $derived(!canUpdate);
  const disabled = $derived(row.is_magic);
  const canEditValue = $derived(canUpdate && !locked && !disabled && !hidden);
  const showFlags = $derived(!row.is_redis_credential && !row.is_magic);
  const requiredBadge = $derived(loaded ? reallyRequired : row.is_required);
  const composeInfo = $derived(row.compose_info);
  const warning = $derived(buildWarning(key, value, isBuildtime));
  const snapshot = () => JSON.stringify([key, comment, value, newLockedValue, isMultiline, isLiteral, isBuildtime, isRuntime]);
  const dirty = $derived(loaded && snapshot() !== initial);

  function fill() {
    key = row.key;
    comment = row.comment ?? '';
    isMultiline = row.is_multiline;
    isLiteral = row.is_literal;
    isBuildtime = row.is_buildtime;
    isRuntime = row.is_runtime;
  }

  async function openEditor() {
    open = true;
    loaded = false;
    error = null;
    fill();
    value = '';
    newLockedValue = '';
    if (!hidden) {
      const r = await api.values(row.id);
      value = r.value ?? '';
      realValue = r.real_value;
      isShared = r.is_shared;
      reallyRequired = r.is_really_required;
      legacy = r.legacy_escaping_changes_value;
    }
    loaded = true;
    initial = snapshot();
  }

  $effect(() => {
    if (openKey && openKey === row.key && !open) {
      root?.scrollIntoView({ behavior: 'smooth', block: 'center' });
      openEditor();
    }
  });

  async function copyValue() {
    const r = await api.copy(row.id);
    await navigator.clipboard?.writeText(r.value ?? '').catch(() => {});
    copied = true;
    setTimeout(() => (copied = false), 1500);
  }

  const payload = () => ({
    key,
    value: locked ? null : value,
    new_locked_value: locked && newLockedValue !== '' ? newLockedValue : null,
    comment: comment === '' && row.comment === null ? null : comment,
    is_multiline: isMultiline,
    is_literal: isLiteral,
    is_runtime: isRuntime,
    is_buildtime: isBuildtime,
  });

  function failure(err: unknown) {
    const f = err instanceof ApiError ? err : new ApiError(0, String(err));
    if (Object.keys(f.errors).length) error = f;
    else notify.failure(f.message);
  }

  async function submit(e?: Event) {
    e?.preventDefault();
    error = null;
    try {
      const r = await api.update(row.id, payload());
      notify.success(r.message);
      open = false;
      onchange();
    } catch (err) {
      failure(err);
    }
  }

  async function useComposeDefault() {
    error = null;
    try {
      const r = await api.composeDefault(row.id, payload());
      if (r.message) notify.success(r.message);
      open = false;
      onchange();
    } catch (err) {
      failure(err);
    }
  }

  async function lock() {
    try {
      await api.lock(row.id);
      confirmingLock = false;
      onchange();
    } catch (err) {
      confirmingLock = false;
      failure(err);
    }
  }

  async function remove() {
    try {
      const r = await api.destroy(row.id);
      notify.success(r.message);
      confirming = false;
      open = false;
      onchange();
    } catch (err) {
      notify.failure(err instanceof ApiError ? err.message : String(err));
      confirming = false;
    }
  }
</script>

{#snippet check()}
  <span class="data-table-cell-check">
    <svg xmlns="http://www.w3.org/2000/svg" fill="none" viewBox="0 0 24 24" stroke-width="2" stroke="currentColor" class="size-4">
      <path stroke-linecap="round" stroke-linejoin="round" d="m4.5 12.75 6 6 9-13.5" />
    </svg>
  </span>
{/snippet}

<div class="env-table-item" data-env-key={row.key} bind:this={root}>
  <div class="data-table-row env-table-grid env-table-grid-no-type">
    <div class="flex min-w-0 items-center gap-2">
      {#if locked}
        <svg class="size-3.5 shrink-0 text-neutral-400 dark:text-fg-faint" viewBox="0 0 24 24" xmlns="http://www.w3.org/2000/svg">
          <g fill="none" stroke="currentColor" stroke-linecap="round" stroke-linejoin="round" stroke-width="2">
            <path d="M5 13a2 2 0 0 1 2-2h10a2 2 0 0 1 2 2v6a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2v-6z" />
            <path d="M11 16a1 1 0 1 0 2 0a1 1 0 0 0-2 0m-3-5V7a4 4 0 1 1 8 0v4" />
          </g>
        </svg>
      {/if}
      <button type="button" data-env-name-trigger class="env-key-label min-w-0 truncate text-left font-mono text-[13px] text-black dark:text-fg" title={row.key} onclick={openEditor}>
        {row.key}
      </button>
      {#if row.comment}<Helper helper={row.comment} />{/if}
      {#if requiredBadge}<span class="table-badge table-badge-danger shrink-0">Required</span>{/if}
      {#if composeInfo}
        <span class="table-badge shrink-0" title="Used by Compose services: {composeInfo.services.join(', ')}">Used by Compose</span>
      {/if}
    </div>
    {#if row.is_magic}
      <span class="env-managed-desktop data-table-cell-check">
        <svg xmlns="http://www.w3.org/2000/svg" fill="none" viewBox="0 0 24 24" stroke-width="2" stroke="currentColor" class="size-4">
          <path stroke-linecap="round" stroke-linejoin="round" d="m4.5 12.75 6 6 9-13.5" />
        </svg>
      </span>
    {:else}
      <span class="env-managed-desktop data-table-cell-dash">-</span>
    {/if}
    {#each [row.is_literal, row.is_multiline, row.is_buildtime && !row.is_redis_credential, row.is_runtime && !row.is_redis_credential] as flag, i (i)}
      {#if flag}{@render check()}{:else}<span class="data-table-cell-dash">-</span>{/if}
    {/each}
    <div class="flex items-center justify-end gap-0.5 justify-self-end">
      {#if !locked && !hidden}
        <button type="button" title="Copy value" aria-label="Copy value" class="icon-button group shrink-0" onclick={copyValue}>
          <span class="inline-flex transition-transform duration-150 ease-out group-active:scale-75">
            {#if copied}<Reicon name="check" class="size-3.5 text-success" />{:else}<Reicon name="copy" class="size-3.5" />{/if}
          </span>
        </button>
      {/if}
      <div class="relative h-auto w-auto">
      <button type="button" data-env-settings-trigger class="icon-button shrink-0" title="Edit environment variable" aria-label="Edit environment variable" onclick={openEditor}>
        <Reicon name="settings" class="size-3.5" />
      </button>
      </div>
    </div>
  </div>
</div>

{#if open}
  <Modal title="Edit environment variable" focusFirstField={false} onclose={() => (open = false)}>
    <form onsubmit={submit} class="flex w-full flex-col gap-4">
      <div class="grid items-end gap-4 sm:grid-cols-2">
        <FormInput id="key" label="Name" required={row.is_redis_credential} disabled={!canEditValue || row.is_redis_credential || composeInfo !== null} bind:value={key} error={error?.field('key')} />
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
          <FormInput
            id="newLockedValue"
            type="password"
            label="Value"
            copyable={false}
            placeholder="Hidden after locking. Enter a new value to replace it."
            helper="The locked value cannot be shown. Leave this empty to keep it."
            disabled={!canUpdate || disabled}
            bind:value={newLockedValue}
          />
        {:else if isMultiline}
          <div class="w-full">
            <!-- svelte-ignore a11y_label_has_associated_control -->
            <label class="mb-1 flex items-center gap-1 text-sm font-medium">Value</label>
            <FormTextarea id="value" disabled={!canEditValue} required={row.is_redis_credential} bind:value />
          </div>
        {:else}
          <div class="w-full">
            <!-- svelte-ignore a11y_label_has_associated_control -->
            <label class="mb-1 flex items-center gap-1 text-sm font-medium">Value</label>
            <div class="relative">
              {#if loaded}
                <EnvVarInput id="value" bind:value {availableVars} disabled={!canEditValue} required={row.is_redis_credential} placeholder={composeInfo?.default ?? ''} />
              {:else}
                <div class="input input-with-password-toggle flex items-center text-neutral-500 dark:text-fg-dim" aria-busy="true">Loading value...</div>
              {/if}
            </div>
          </div>
        {/if}
        {#if !hidden && !locked && !isMultiline}
          <div class="mt-1.5 text-xs text-neutral-500 dark:text-fg-faint">
            Tip: Type <span class="font-mono text-coollabs dark:text-brand-light">{'{{'}</span> to reference a shared environment variable
          </div>
        {/if}
      </div>

      {#if legacy}
        <Callout type="warning" title="Old escaping">
          This variable was saved before coolify-rs passed exact values, so the container gets a
          changed value (for example, with extra backslashes or quotes). To pass the exact value,
          delete this variable and add it again, then restart. If a database already uses this
          value as a password, update the password in the database too.
        </Callout>
      {/if}

      {#if isShared}
        <FormInput id="real_value" label="Resolved value" type="password" disabled value={realValue ?? ''} />
      {/if}
      {#if composeInfo}
        <Callout type="info" title="Used by Docker Compose">
          Used by: {composeInfo.services.join(', ')}.
          {#if composeInfo.default !== null}
            Compose default: <span class="font-mono">{composeInfo.default}</span>.
          {/if}
          {#if composeInfo.default !== null && !composeInfo.required && canEditValue && !locked}
            <button type="button" class="button" onclick={useComposeDefault}>Use Compose default</button>
          {/if}
          {#if composeInfo.required}
            Compose requires this variable to have a value.
          {/if}
        </Callout>
      {/if}

      {#if showFlags}
        <div class="grid gap-4 border-t border-neutral-200 pt-4 dark:border-white/[0.07] sm:grid-cols-2">
          <Listbox id="is_multiline" label="Value type" disabled={!canUpdate} bind:value={isMultiline} options={[{ value: false, label: 'Single line' }, { value: true, label: 'Multiline' }]} />
          <Listbox
            id="is_literal"
            label="Interpolation"
            disabled={!canUpdate}
            bind:value={isLiteral}
            helper="Literal means $VARIABLES in the value is kept as the actual characters '$VARIABLES' instead of being resolved from another variable. Useful when your value contains a $ sign."
            options={[{ value: false, label: 'Interpolate $VARIABLES' }, { value: true, label: 'Literal (keep $ characters as-is)' }]}
          />
          <Listbox
            id="is_buildtime"
            label="Build time"
            disabled={!canUpdate}
            bind:value={isBuildtime}
            helper="Make this variable available during the Docker build process. Useful for build secrets and dependencies."
            options={[{ value: true, label: 'Available during build' }, { value: false, label: 'Not available during build' }]}
          />
          <Listbox
            id="is_runtime"
            label="Runtime"
            disabled={!canUpdate}
            bind:value={isRuntime}
            helper="Make this variable available in the running container at runtime."
            options={[{ value: true, label: 'Available in the container' }, { value: false, label: 'Not available in the container' }]}
          />
        </div>
      {/if}

      {#if warning}
        <Callout type="warning" title="Caution">
          <div class="text-sm">{warning.issue}</div>
          <div class="text-sm">Recommendation: {warning.recommendation}</div>
        </Callout>
      {/if}

      {#if canUpdate}
        <div class="flex flex-wrap items-center justify-between gap-2 border-t border-neutral-200 pt-4 dark:border-white/[0.07]">
          <div data-environment-variable-delete-action>
            {#if !row.is_magic}
              <button type="button" class="button button-error" onclick={() => (confirming = true)}>Delete</button>
            {/if}
          </div>
          <div class="ml-auto flex flex-wrap gap-2" data-environment-variable-update-actions>
            {#if !locked && !row.is_magic}
              <button type="button" class="button" onclick={() => (confirmingLock = true)}>Lock</button>
            {/if}
            {#if !disabled}
              <fieldset disabled={!dirty}>
                <button type="submit" class="button" {...{ isHighlighted: '' }}>Update variable</button>
              </fieldset>
            {/if}
          </div>
        </div>
      {/if}
    </form>
  </Modal>
{/if}

{#if confirmingLock}
  <ConfirmModal
    title="Lock Environment Variable?"
    actions={['The value will be hidden. You cannot see or copy it again.', 'You can still replace the value with a new one, but you cannot unlock the variable.']}
    confirmWithText={false}
    confirmWithPassword={false}
    step2ButtonText="Lock"
    onconfirm={async () => {
      await lock();
    }}
    onclose={() => (confirmingLock = false)}
  />
{/if}

{#if confirming}
  <ConfirmModal
    title="Confirm Environment Variable Deletion?"
    actions={['The selected environment variable will be permanently deleted.']}
    confirmationText={row.key}
    confirmationLabel="Please confirm the execution of the actions by entering the Environment Variable Name below"
    shortConfirmationLabel="Environment Variable Name"
    step2ButtonText="Permanently Delete"
    onconfirm={async () => {
      await remove();
    }}
    onclose={() => (confirming = false)}
  />
{/if}
