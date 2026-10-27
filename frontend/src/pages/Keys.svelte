<script lang="ts">
  import Icon from '../components/Icon.svelte';
  import Modal from '../components/Modal.svelte';
  import ConfirmDialog from '../components/ConfirmDialog.svelte';
  import { ApiError, api, type PrivateKey } from '../lib/api';
  import { session } from '../lib/session.svelte';
  import { notify } from '../lib/toast.svelte';
  import { focusOnMount } from '../lib/focus';

  let keys = $state<PrivateKey[] | null>(null);
  let loadError = $state<ApiError | null>(null);
  let editor = $state<{ key: PrivateKey | null } | null>(null);
  let deleting = $state<PrivateKey | null>(null);

  // Editor form state
  let name = $state('');
  let description = $state('');
  let privateKey = $state('');
  let busy = $state(false);
  let formError = $state<ApiError | null>(null);

  async function load() {
    try {
      keys = await api.get<PrivateKey[]>('/security/keys');
      loadError = null;
    } catch (e) {
      loadError = e instanceof ApiError ? e : new ApiError(0, String(e));
    }
  }

  $effect(() => {
    session.info?.current_team?.id;
    load();
  });

  function open(key: PrivateKey | null) {
    name = key?.name ?? '';
    description = key?.description ?? '';
    privateKey = '';
    formError = null;
    editor = { key };
  }

  async function save() {
    busy = true;
    formError = null;
    try {
      const body: Record<string, string> = {};
      if (name.trim()) body.name = name;
      if (description.trim()) body.description = description;
      if (privateKey.trim()) body.private_key = privateKey;
      if (editor?.key) {
        await api.patch(`/security/keys/${editor.key.uuid}`, body);
        notify.ok('Private key updated');
      } else {
        await api.post('/security/keys', body);
        notify.ok('Private key added');
      }
      editor = null;
      await load();
    } catch (e) {
      formError = e instanceof ApiError ? e : new ApiError(0, String(e));
    } finally {
      busy = false;
    }
  }

  async function copy(text: string) {
    try {
      await navigator.clipboard.writeText(text.trim());
      notify.ok('Public key copied');
    } catch {
      notify.error('Could not copy to the clipboard');
    }
  }
</script>

<div class="page-head">
  <div>
    <h1>Private keys</h1>
    <div class="summary">SSH keys Coolify uses to connect to servers and Git repositories.</div>
  </div>
  {#if session.isAdmin && session.info?.features.private_keys}
    <button class="button button-primary" onclick={() => open(null)}><Icon name="plus" /> Add key</button>
  {/if}
</div>

<div class="card">
  <div class="card-well is-flush">
    {#if loadError}
      <div class="empty"><div class="empty-title">Could not load private keys</div><div class="empty-text">{loadError.message}</div></div>
    {:else if keys === null}
      <div class="empty"><div class="empty-text">Loading…</div></div>
    {:else if keys.length === 0}
      <div class="empty">
        <div class="empty-title">No private keys</div>
        <div class="empty-text">Add a key to let Coolify reach your servers over SSH.</div>
      </div>
    {:else}
      <table class="table">
        <thead><tr><th>Name</th><th>Fingerprint (SHA256)</th><th>Description</th><th class="actions"></th></tr></thead>
        <tbody>
          {#each keys as k (k.uuid)}
            <tr>
              <td class="cell-strong">
                {k.name}
                {#if k.is_git_related}<span class="badge" style="margin-left:6px"><span class="badge-dot"></span>Git</span>{/if}
              </td>
              <td class="mono cell-faint truncate" title={k.fingerprint ?? ''}>{k.fingerprint ?? 'Unknown'}</td>
              <td class="cell-faint truncate">{k.description || 'No description'}</td>
              <td class="actions">
                <button class="button button-ghost" onclick={() => copy(k.public_key)} title="Copy public key">
                  <Icon name="copy" /> Public key
                </button>
                {#if session.isAdmin}
                  <button class="button button-ghost" onclick={() => open(k)}>Edit</button>
                  <button class="button button-ghost button-danger" onclick={() => (deleting = k)}>Delete</button>
                {/if}
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    {/if}
  </div>
</div>

{#if editor}
  <Modal
    title={editor.key ? 'Edit private key' : 'Add private key'}
    description={editor.key ? 'Leave the key empty to keep the current one.' : 'OpenSSH or PEM (RSA) private keys without a passphrase.'}
    onclose={() => (editor = null)}
  >
    <div class="field">
      <label class="label" for="k-name">Name</label>
      <input id="k-name" class="input" use:focusOnMount bind:value={name} placeholder={editor.key ? '' : 'Generated if empty'} />
      {#if formError?.field('name')}<div class="field-error">{formError.field('name')}</div>{/if}
    </div>
    <div class="field">
      <label class="label" for="k-desc">Description</label>
      <input id="k-desc" class="input" bind:value={description} placeholder="Optional" />
      {#if formError?.field('description')}<div class="field-error">{formError.field('description')}</div>{/if}
    </div>
    <div class="field">
      <label class="label" for="k-key">Private key</label>
      <textarea id="k-key" class="textarea" bind:value={privateKey} spellcheck="false" autocomplete="off"
        placeholder="-----BEGIN OPENSSH PRIVATE KEY-----"></textarea>
      {#if formError?.field('private_key')}<div class="field-error">{formError.field('private_key')}</div>{/if}
    </div>
    {#if editor.key}
      <div class="field">
        <span class="label">Public key</span>
        <div class="code">{editor.key.public_key}</div>
      </div>
    {/if}
    {#if formError && Object.keys(formError.errors).length === 0}
      <div class="notice notice-error" style="margin-top:16px">{formError.message}</div>
    {/if}
    {#snippet actions()}
      <button class="button" onclick={() => (editor = null)} disabled={busy}>Cancel</button>
      <button class="button button-primary" onclick={save} disabled={busy}>{editor?.key ? 'Save' : 'Add key'}</button>
    {/snippet}
  </Modal>
{/if}

{#if deleting}
  {@const k = deleting}
  <ConfirmDialog
    title="Delete private key"
    message={`Delete "${k.name}"? Keys used by servers, applications or Git integrations cannot be deleted.`}
    onconfirm={async () => {
      await api.del(`/security/keys/${k.uuid}`);
      notify.ok('Private key deleted', k.name);
      await load();
    }}
    onclose={() => (deleting = null)}
  />
{/if}
