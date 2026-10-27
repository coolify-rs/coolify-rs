<script lang="ts">
  import Modal from './Modal.svelte';
  import { ApiError } from '../lib/api';
  import { focusOnMount } from '../lib/focus';

  // Shared create/edit form for projects and environments. The API does all
  // validation; this form only shows its messages next to the fields.
  let {
    title,
    submitLabel,
    initial = { name: '', description: '' },
    withDescription = true,
    onsubmit,
    onclose,
  }: {
    title: string;
    submitLabel: string;
    initial?: { name: string; description: string };
    withDescription?: boolean;
    onsubmit: (v: { name: string; description: string }) => Promise<void>;
    onclose: () => void;
  } = $props();

  // The form edits a copy of the initial values on purpose.
  // svelte-ignore state_referenced_locally
  let name = $state(initial.name);
  // svelte-ignore state_referenced_locally
  let description = $state(initial.description);
  let busy = $state(false);
  let error = $state<ApiError | null>(null);

  async function submit(e?: Event) {
    e?.preventDefault();
    busy = true;
    error = null;
    try {
      await onsubmit({ name, description });
      onclose();
    } catch (err) {
      error = err instanceof ApiError ? err : new ApiError(0, String(err));
    } finally {
      busy = false;
    }
  }
</script>

<Modal {title} {onclose}>
  <form onsubmit={submit}>
    <div class="field">
      <label class="label" for="pf-name">Name</label>
      <input id="pf-name" class="input" bind:value={name} use:focusOnMount />
      {#if error?.field('name')}<div class="field-error">{error.field('name')}</div>{/if}
    </div>
    {#if withDescription}
      <div class="field">
        <label class="label" for="pf-desc">Description</label>
        <input id="pf-desc" class="input" bind:value={description} placeholder="Optional" />
        {#if error?.field('description')}<div class="field-error">{error.field('description')}</div>{/if}
      </div>
    {/if}
    {#if error && Object.keys(error.errors).length === 0}
      <div class="notice notice-error" style="margin-top:16px">{error.message}</div>
    {/if}
    <button type="submit" hidden aria-hidden="true"></button>
  </form>
  {#snippet actions()}
    <button class="button" onclick={onclose} disabled={busy}>Cancel</button>
    <button class="button button-primary" onclick={() => submit()} disabled={busy}>{submitLabel}</button>
  {/snippet}
</Modal>
