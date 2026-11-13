<script lang="ts">
  // Port of upstream livewire/security/private-key/rotate.blade.php (`Security\PrivateKey\Rotate`):
  // moves the servers of a private key to a new key.
  import Listbox from './Listbox.svelte';
  import Modal from './Modal.svelte';
  import StatusBadge from './StatusBadge.svelte';
  import { failure } from '../lib/serverApi';
  import { loadRotation, rotateKey, type RotationData } from '../lib/keyRotationApi';
  import { notify } from '../lib/toast.svelte';

  let {
    uuid,
    canDelete = false,
    onrotated,
    ondeleteoldkey,
  }: {
    uuid: string;
    canDelete?: boolean;
    /** `$this->dispatch('privateKeyRotated')->to(Show::class)`: the "Used by" list changed. */
    onrotated?: () => void;
    /** `$dispatch('confirm-old-key-deletion', { id })`. */
    ondeleteoldkey?: () => void;
  } = $props();

  let data = $state<RotationData | null>(null);
  let open = $state(false);
  let wasRunning = $state(false);
  let starting = $state(false);
  let keyType = $state<'ed25519' | 'rsa'>('ed25519');

  function apply(d: RotationData) {
    if (wasRunning && !d.is_running) onrotated?.();
    wasRunning = d.is_running;
    data = d;
  }

  async function load() {
    try {
      apply(await loadRotation(uuid));
    } catch (e) {
      failure(e);
    }
  }

  $effect(() => {
    void uuid;
    load();
  });

  // `wire:poll.3s` while the rotation runs.
  $effect(() => {
    if (!data?.is_running) return;
    const timer = setInterval(load, 3000);
    return () => clearInterval(timer);
  });

  async function rotate() {
    starting = true;
    try {
      const r = await rotateKey(uuid, keyType);
      apply(r);
      wasRunning = true;
      if (r.message) notify.success(r.message);
    } catch (e) {
      failure(e);
    } finally {
      starting = false;
    }
  }
</script>

{#if data && (data.has_servers || data.is_running || open || data.state)}
  <button type="button" class="button" onclick={() => (open = true)}>
    {data.has_servers || data.is_running ? 'Rotate key' : 'Rotation result'}
  </button>
{/if}

{#if open && data}
  <Modal title="Rotate key" subtitle="Move the servers to a new key. Servers that fail stay on this key." onclose={() => (open = false)}>
    <div class="flex flex-col gap-3 text-sm">
      <ul class="flex flex-col divide-y divide-neutral-200 dark:divide-white/[0.06]">
        {#each data.rows as row (row.uuid)}
          <li class="flex flex-col gap-0.5 py-2">
            <span class="flex items-center gap-2">
              <span>{row.name}</span>
              <span class="ml-auto">
                {#if row.status === 'moved' || row.status === 'moved_old_pending'}
                  <StatusBadge size="sm" label="Moved" type="success" />
                {:else if row.status === 'moved_old_not_removed'}
                  <StatusBadge size="sm" label="Moved" type="warning" />
                {:else if row.status === 'failed'}
                  <StatusBadge size="sm" label="Failed" type="error" />
                {:else if row.status === 'old_key_not_found'}
                  <StatusBadge size="sm" label="Old key not found" type="error" />
                {:else if row.status === 'unreachable'}
                  <StatusBadge size="sm" label="Unreachable" type="error" />
                {:else if row.status === 'skipped'}
                  <StatusBadge size="sm" label="Transferred" type="warning" />
                {:else if row.transferred}
                  <StatusBadge size="sm" label="Transferred" type="warning" />
                {:else if data.is_running}
                  <StatusBadge size="sm" label="Waiting" pulse />
                {:else if !row.reachable}
                  <StatusBadge size="sm" label="Not reachable" type="warning" />
                {/if}
              </span>
            </span>
            {#if row.status !== null && row.status !== undefined && row.status !== 'moved'}
              <span class="text-xs text-neutral-500 dark:text-fg-dim">{row.message}</span>
            {:else if (row.status === null || row.status === undefined) && row.transferred}
              <span class="text-xs text-neutral-500 dark:text-fg-dim">Another Coolify instance manages this server now.</span>
            {/if}
          </li>
        {/each}
      </ul>

      {#if !data.state}
        <p class="text-xs text-neutral-500 dark:text-fg-dim">Rotation progress and logs will appear here.</p>
      {/if}
      {#if data.has_servers && !data.is_running && !data.new_key}
        <Listbox
          id="keyType"
          label="New key type"
          bind:value={keyType}
          options={[
            { value: 'ed25519', label: 'Ed25519', description: 'Recommended.' },
            { value: 'rsa', label: 'RSA 4096', description: 'For hosts that do not accept Ed25519, for example FIPS hosts.' },
          ]}
        />
      {/if}
      {#if data.state?.status === 'failed'}
        <p class="text-xs text-red-500">{data.state.error}</p>
      {/if}
      {#if data.new_key}
        <p class="text-xs text-neutral-500 dark:text-fg-dim">
          New key:
          <span class="font-medium">{data.new_key.name}</span>
        </p>
      {/if}
      {#if data.used_by_others}
        <p class="text-xs text-neutral-500 dark:text-fg-dim">Applications and Git sources stay on this key.</p>
      {:else if !data.is_running && !data.has_servers}
        <p class="text-xs text-neutral-500 dark:text-fg-dim">Nothing uses this key now. You can delete it.</p>
      {/if}
      <div class="flex items-center justify-end gap-2 border-t border-neutral-200 pt-4 dark:border-white/[0.08]">
        {#if !data.is_running && !data.has_servers && !data.used_by_others && canDelete}
          <button
            type="button"
            class="button button-error"
            onclick={() => {
              open = false;
              ondeleteoldkey?.();
            }}
          >
            Delete old key
          </button>
        {/if}
        {#if data.has_servers || data.is_running}
          <button type="button" class="button" {...{ ishighlighted: '' }} onclick={rotate} disabled={data.is_running || starting}>
            {data.is_running ? 'Rotating...' : data.state ? 'Rotate again' : 'Rotate key'}
          </button>
        {/if}
      </div>
    </div>
  </Modal>
{/if}
