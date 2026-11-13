<script lang="ts" module>
  export interface IgnoreTarget {
    target: string;
    kind: string;
    owner: string | null;
    neverAllowed: boolean;
  }
</script>

<script lang="ts">
  // Port of upstream livewire/image-scan/partials/ignore-modal.blade.php: one modal for every
  // Ignore button. Validation errors arrive as an error toast; the modal stays open so the input
  // can be fixed.
  import Modal from '../Modal.svelte';
  import Listbox from '../Listbox.svelte';
  import { notify } from '../../lib/toast.svelte';
  import { ApiError } from '../../lib/api';

  let {
    detail,
    canIgnoreForTeam,
    onsubmit,
    onclose,
  }: {
    detail: IgnoreTarget;
    canIgnoreForTeam: boolean;
    /** `$wire.ignore(...)`: true when saved. */
    onsubmit: (body: { target: string; kind: string; scope: string; expires: string; reason: string; owner: string | null }) => Promise<boolean>;
    onclose: () => void;
  } = $props();

  let scope = $state('resource');
  let expires = $state('90');
  let reason = $state('');
  let saving = $state(false);

  async function submit() {
    if (saving || reason.trim() === '') return;
    saving = true;
    try {
      const saved = await onsubmit({ target: detail.target, kind: detail.kind, scope, expires, reason, owner: detail.owner });
      if (saved) onclose();
    } catch (e) {
      if (e instanceof ApiError && e.status === 422) notify.failure(e.message);
      else notify.error('Could not ignore', 'You may not have permission to do this. Reload the page and try again.');
    } finally {
      saving = false;
    }
  }
</script>

<Modal title={detail.kind === 'package' ? 'Ignore package' : 'Ignore finding'} {onclose}>
  <form
    class="flex flex-col gap-4"
    onsubmit={(e) => {
      e.preventDefault();
      void submit();
    }}
  >
    <p class="break-all font-mono text-[12px] text-neutral-600 dark:text-fg-dim">{detail.target}</p>
    <div class="flex-1 form-control">
      <label for="image-scan-ignore-reason" class="mb-1.5 flex items-center gap-1 text-sm font-medium leading-4">
        Reason <span class="text-helper">*</span>
      </label>
      <textarea
        id="image-scan-ignore-reason"
        bind:value={reason}
        rows="3"
        maxlength="500"
        required
        class="input scrollbar"
        placeholder="Why this does not affect the app"
      ></textarea>
    </div>
    <div class="grid gap-4 sm:grid-cols-2">
      <Listbox
        id="image-scan-ignore-scope"
        label="Scope"
        bind:value={scope}
        options={[
          { value: 'resource', label: 'This resource' },
          ...(canIgnoreForTeam ? [{ value: 'team', label: 'All team resources' }] : []),
        ]}
      />
      <!-- Ignores of problems that attackers are using or are likely to use always expire (spec 15.2). -->
      {#if detail.neverAllowed}
        <Listbox
          id="image-scan-ignore-expires"
          label="Expires"
          bind:value={expires}
          options={[
            { value: '30', label: '30 days' },
            { value: '90', label: '90 days' },
            { value: 'never', label: 'Never' },
          ]}
        />
      {:else}
        <Listbox
          id="image-scan-ignore-expires"
          label="Expires"
          bind:value={expires}
          helper="Attackers are using or are likely to use this problem, so its ignore expires."
          options={[
            { value: '30', label: '30 days' },
            { value: '90', label: '90 days' },
          ]}
        />
      {/if}
    </div>
  </form>
  {#snippet footer()}
    <button type="button" class="button normal-case!" onclick={onclose}>Cancel</button>
    <button type="button" class="button button-highlighted normal-case!" onclick={() => void submit()} disabled={saving || reason.trim() === ''}>Ignore</button>
  {/snippet}
</Modal>
