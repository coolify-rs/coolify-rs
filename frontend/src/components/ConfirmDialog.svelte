<script lang="ts">
  import Modal from './Modal.svelte';
  import { ApiError } from '../lib/api';

  let {
    title,
    message,
    confirmLabel = 'Delete',
    onconfirm,
    onclose,
  }: {
    title: string;
    message: string;
    confirmLabel?: string;
    onconfirm: () => Promise<void>;
    onclose: () => void;
  } = $props();

  let busy = $state(false);
  let error = $state('');

  async function confirm() {
    busy = true;
    error = '';
    try {
      await onconfirm();
      onclose();
    } catch (e) {
      error = e instanceof ApiError ? [e.message, e.details].filter(Boolean).join(' ') : String(e);
    } finally {
      busy = false;
    }
  }
</script>

<Modal {title} {onclose}>
  <p style="margin:0">{message}</p>
  {#if error}<div class="notice notice-error" style="margin-top:12px">{error}</div>{/if}
  {#snippet actions()}
    <button class="button" onclick={onclose} disabled={busy}>Cancel</button>
    <button class="button button-danger" onclick={confirm} disabled={busy}>{confirmLabel}</button>
  {/snippet}
</Modal>
