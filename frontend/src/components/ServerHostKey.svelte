<script lang="ts">
  // Port of upstream livewire/server/security/host-key.blade.php (`Server\Security\HostKey`):
  // the SSH host key section of the server Security page.
  import Callout from './Callout.svelte';
  import ConfirmModal from './ConfirmModal.svelte';
  import Listbox from './Listbox.svelte';
  import Reicon from './Reicon.svelte';
  import SettingsSection from './SettingsSection.svelte';
  import StatusBadge from './StatusBadge.svelte';
  import { ApiError } from '../lib/api';
  import { failure } from '../lib/serverApi';
  import {
    loadHostKey,
    resetHostKey,
    setHostKeyCheckDisabled,
    setHostKeyCheckMode,
    trustPendingKey,
    type HostKeyData,
  } from '../lib/serverHostKeyApi';
  import { notify } from '../lib/toast.svelte';

  let { uuid, onchanged }: { uuid: string; onchanged?: () => void } = $props();

  let data = $state<HostKeyData | null>(null);
  let confirm = $state({ text: true, password: true });
  let checkMode = $state<'warn' | 'enforce'>('warn');
  let modal = $state<'trust' | 'reset' | 'check' | null>(null);

  function apply(d: HostKeyData) {
    if (d.confirm) confirm = d.confirm;
    data = d;
    checkMode = d.check_mode;
  }

  export async function load() {
    try {
      apply(await loadHostKey(uuid));
    } catch (e) {
      failure(e);
    }
  }

  $effect(() => {
    void uuid;
    load();
  });

  /** A confirmed action: the wrong password stays in the modal, as upstream returns it. */
  async function run(action: (password: string) => Promise<HostKeyData>, password: string): Promise<string | void> {
    try {
      const r = await action(password);
      apply(r);
      if (r.message) notify.success(r.message);
      // `$this->dispatch('hostKeySettingsChanged')->to(Index::class)`.
      onchanged?.();
    } catch (e) {
      if (e instanceof ApiError && e.message === 'The provided password is incorrect.') return e.message;
      throw e;
    }
  }

  async function changeMode() {
    try {
      const r = await setHostKeyCheckMode(uuid, checkMode);
      if ('saved' in r) return;
      apply(r);
      if (r.message) notify.success(r.message);
      onchanged?.();
    } catch (e) {
      failure(e);
      if (data) checkMode = data.check_mode;
    }
  }
</script>

{#if data}
  <div class="application-settings-form flex w-full flex-col gap-6">
    {#if data.pending_fingerprints.length > 0}
      <Callout type="danger" title="The SSH host key of this server changed">
        {#each data.pending_fingerprints as fingerprint (fingerprint)}
          <code class="block break-all">{fingerprint}</code>
        {/each}
        <p class="mt-2">
          {data.mode === 'enforce' ? 'Coolify does not connect until you trust or reset the key.' : 'Coolify still connects (warn mode).'}
          Expected after a reinstall. Otherwise, compare the key on the server before you trust it.
        </p>
      </Callout>
    {/if}

    <SettingsSection
      id="server-host-key-section"
      title="SSH host key"
      description="Coolify trusts the first host key of the server. Later, a different key is reported (warn mode, the default) or refused (enforce mode). The setting below sets enforce mode for this server only.<br><br>An instance admin sets the mode for all servers with <code>SSH_HOST_KEY_CHECKING=enforce</code> (or <code>warn</code>, <code>off</code>) in <code>/data/coolify/source/.env</code>, then restarts Coolify."
    >
      {#snippet badges()}
        {#if data && data.pending_fingerprints.length > 0}
          <StatusBadge label="Key changed" type="error" />
        {:else if data && data.mode === 'off'}
          <StatusBadge label="Check off" type="warning" />
        {:else if data && data.fingerprints.length > 0}
          <StatusBadge label={'Trusted, ' + data.mode + ' mode'} type="success" />
        {:else}
          <StatusBadge label="No key yet" type="neutral" />
        {/if}
      {/snippet}
      {#snippet actions()}
        {#if data && data.can_update}
          <div class="flex flex-wrap items-center gap-2">
            {#if data.pending_fingerprints.length > 0}
              <button type="button" class="button button-highlighted" onclick={() => (modal = 'trust')}>Trust new key</button>
            {/if}
            <button type="button" class="button button-error" onclick={() => (modal = 'reset')}>Reset</button>
            <button type="button" class="button {data.is_check_disabled ? '' : 'button-error'}" onclick={() => (modal = 'check')}>
              {data.is_check_disabled ? 'Enable check' : 'Disable check'}
            </button>
          </div>
        {/if}
      {/snippet}

      <div class="flex flex-col gap-3 text-sm">
        {#if data.mode === 'off'}
          <Callout type="warning" title="The host key check is off">
            {data.is_check_disabled ? 'Disabled for this server.' : 'Disabled for this instance (SSH_HOST_KEY_CHECKING=off).'}
          </Callout>
        {/if}

        <div class="flex min-w-0 items-start gap-3">
          <div class="flex size-9 shrink-0 items-center justify-center rounded-lg bg-neutral-100 text-neutral-500 dark:bg-white/[0.06] dark:text-fg-dim">
            <Reicon name="keys" class="size-4" />
          </div>
          <div class="min-w-0">
            {#each data.fingerprints as fingerprint (fingerprint)}
              <code class="block break-all text-xs">{fingerprint}</code>
            {:else}
              <p class="font-medium text-neutral-950 dark:text-fg">No key yet</p>
            {/each}
            <p class="mt-1 text-xs text-neutral-500 dark:text-fg-dim">
              {#if data.fingerprints.length > 0}
                {data.source_label}
                {#if data.trusted_at}
                  &middot; Trusted {data.trusted_at} UTC
                {/if}
                &middot;
              {:else}
                Saved on the next connection &middot;
              {/if}
              Mode: {data.mode}{data.mode === 'enforce' && data.instance_mode !== 'enforce' ? ' (set for this server)' : ''}
            </p>
          </div>
        </div>

        <!-- The choice only changes something when the instance warns; an instance "enforce" or "off" decides alone. -->
        {#if data.instance_mode !== 'enforce' && data.instance_mode !== 'off' && !data.is_check_disabled}
          <div class="max-w-sm">
            <Listbox
              id="checkMode"
              label="When the host key changes"
              disabled={!data.can_update}
              helper="Warn: Coolify connects and reports the new key. Enforce: Coolify refuses to connect until you trust or reset the new key, so deployments, backups, and checks on this server stop. A reinstall of the server also changes the key."
              bind:value={checkMode}
              onchange={changeMode}
              options={[
                { value: 'warn', label: 'Warn and connect' },
                { value: 'enforce', label: 'Refuse to connect (enforce)' },
              ]}
            />
          </div>
        {/if}
      </div>
    </SettingsSection>
  </div>
{/if}

{#if modal === 'trust' && data && data.pending_key_hash}
  {@const hash = data.pending_key_hash}
  <ConfirmModal
    title="Trust the new host key?"
    actions={['Coolify trusts the new host key and stops trusting the old key.', 'Only do this after you compared the new key with the key on the server.']}
    confirmWithText={confirm.text}
    confirmWithPassword={confirm.password}
    confirmationText={data.server_name}
    shortConfirmationLabel="Server Name"
    step3ButtonText="Trust New Key"
    onconfirm={(password) => run((p) => trustPendingKey(uuid, hash, p), password)}
    onclose={() => (modal = null)}
  />
{:else if modal === 'reset' && data}
  <ConfirmModal
    title="Reset the host key?"
    actions={['Coolify removes the trusted key and the new key.', 'Coolify trusts the key that the server sends on the next connection.']}
    confirmWithText={confirm.text}
    confirmWithPassword={confirm.password}
    confirmationText={data.server_name}
    shortConfirmationLabel="Server Name"
    step3ButtonText="Reset Host Key"
    onconfirm={(password) => run((p) => resetHostKey(uuid, p), password)}
    onclose={() => (modal = null)}
  />
{:else if modal === 'check' && data}
  {@const disable = !data.is_check_disabled}
  <ConfirmModal
    title={data.is_check_disabled ? 'Enable the host key check?' : 'Disable the host key check?'}
    actions={data.is_check_disabled
      ? ['Coolify checks the host key of this server again.']
      : [
          'Coolify does not check the host key of this server.',
          'Use this only for a server that changes its host key often, for example a load balancer address or a server that you rebuild often.',
        ]}
    confirmWithText={confirm.text}
    confirmWithPassword={confirm.password}
    confirmationText={data.server_name}
    shortConfirmationLabel="Server Name"
    step3ButtonText="Confirm"
    onconfirm={(password) => run((p) => setHostKeyCheckDisabled(uuid, disable, p), password)}
    onclose={() => (modal = null)}
  />
{/if}
