<script lang="ts">
  // Port of upstream livewire/storage/sftp-login-fields.blade.php: the login of an SFTP storage, a
  // password or a private key of the storage team.
  import FormInput from './FormInput.svelte';
  import Listbox from './Listbox.svelte';
  import CopyInput from './CopyInput.svelte';
  import type { PrivateKeyOption } from '../lib/storagesApi';

  let {
    auth = $bindable(),
    privateKeyId = $bindable(),
    secret = $bindable(),
    privateKeys,
    hasSavedSecret = false,
    disabled = false,
  }: {
    /** "password" or "key". */
    auth: string;
    privateKeyId: number | null;
    secret: string;
    privateKeys: PrivateKeyOption[];
    hasSavedSecret?: boolean;
    disabled?: boolean;
  } = $props();

  // `selectedPublicKey()`: the public key to add to authorized_keys on the SFTP server.
  const publicKey = $derived(auth === 'key' ? (privateKeys.find((k) => k.value === privateKeyId)?.public_key ?? null) : null);
</script>

<div class="grid gap-4 lg:grid-cols-2">
  <Listbox
    required
    id="sftpAuth"
    label="Authentication"
    {disabled}
    bind:value={auth}
    options={[
      { value: 'key', label: 'Private key' },
      { value: 'password', label: 'Password' },
    ]}
  />
  {#if auth === 'key'}
    <!-- The listbox holds strings; the form sends the ID. -->
    <Listbox
      required
      id="privateKeyId"
      label="Private key"
      {disabled}
      bind:value={() => (privateKeyId === null ? '' : String(privateKeyId)), (v) => (privateKeyId = v === '' ? null : Number(v))}
      options={privateKeys.map((k) => ({ value: String(k.value), label: k.label }))}
      placeholder="Select a private key"
      emptyText="No private keys yet. Add one under Keys & Tokens."
    />
    {#if publicKey}
      <div class="lg:col-span-2">
        <CopyInput label="Public key" text={publicKey} />
        <p class="mt-1 text-[12px] text-neutral-500 dark:text-fg-dim">Add this line to ~/.ssh/authorized_keys of the user on the SFTP server.</p>
      </div>
    {/if}
  {:else}
    <FormInput type="password" label="Password" id="secret" {disabled} required={!hasSavedSecret} placeholder={hasSavedSecret ? 'Saved. Leave empty to keep it.' : ''} bind:value={secret} />
  {/if}
</div>
