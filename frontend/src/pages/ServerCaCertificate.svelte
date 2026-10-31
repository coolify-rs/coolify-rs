<script lang="ts">
  // Port of upstream livewire/server/ca-certificate/show.blade.php.
  import Reicon from '../components/Reicon.svelte';
  import Callout from '../components/Callout.svelte';
  import ConfirmModal from '../components/ConfirmModal.svelte';
  import CopyInput from '../components/CopyInput.svelte';
  import FormTextarea from '../components/FormTextarea.svelte';
  import ServerFrame from '../components/ServerFrame.svelte';
  import SettingsSection from '../components/SettingsSection.svelte';
  import StatusBadge from '../components/StatusBadge.svelte';
  import { get, post, failure } from '../lib/serverApi';
  import { session } from '../lib/session.svelte';
  import { notify } from '../lib/toast.svelte';

  let { uuid }: { uuid: string } = $props();
  let content = $state('');
  let validUntil = $state<string | null>(null);
  let show = $state(false);
  let confirm = $state<'save' | 'regenerate' | null>(null);
  let steps = $state({ text: true, password: true });
  const base = $derived(`/server-page/${encodeURIComponent(uuid)}/ca-certificate`);
  const canEdit = $derived(session.isAdmin);

  async function load() {
    const d = await get<{ certificate: string | null; valid_until: string | null }>(base);
    content = d.certificate ?? '';
    validUntil = d.valid_until;
    const t = await get<{ confirm: { text: boolean; password: boolean } }>(`/server-page/${encodeURIComponent(uuid)}/terminal-access`).catch(() => null);
    if (t) steps = t.confirm;
  }

  const until = $derived(validUntil ? new Date(`${validUntil.replace(' ', 'T')}${validUntil.endsWith('Z') ? '' : 'Z'}`).getTime() : null);
  const now = Date.now();
  const day = 86400000;
  const validity = $derived(until === null ? null : now > until ? 'Expired' : now + 30 * day > until ? 'Expiring soon' : 'Valid');

  async function save() {
    try {
      const r = await post<{ message: string; certificate: string }>(`${base}/save`, { certificate: content });
      content = r.certificate;
      notify.success(r.message);
    } catch (e) {
      failure(e);
    }
  }
  async function regenerate() {
    try {
      const r = await post<{ message: string }>(`${base}/regenerate`);
      await load();
      notify.success(r.message);
    } catch (e) {
      failure(e);
    }
  }
</script>

<ServerFrame {uuid} active="ca-certificate" title="CA Certificate" onloaded={load}>
  {#snippet children()}
    <div class="application-settings-form flex w-full flex-col gap-6">
      <SettingsSection id="server-ca-overview-section" title="CA certificate" description="Manage the certificate authority used to sign database certificates on this server.">
        {#snippet actions()}
          {#if validity}
            <StatusBadge label={validity} type={validity === 'Valid' ? 'success' : 'error'} />
          {/if}
        {/snippet}
        <Callout type="info" title="Using this certificate">
          Mount the CA certificate into containers that connect to databases over SSL. Re-deploy affected databases and resources after replacing or regenerating it.
          <a class="font-medium underline" href="https://coolify.io/docs/databases/ssl" target="_blank">Read the SSL guide.</a>
        </Callout>
        <div class="mt-4">
          <p class="mb-1.5 text-xs font-medium text-neutral-500 dark:text-fg-dim">Read-only bind mount</p>
          <CopyInput text="- /data/coolify/ssl/coolify-ca.crt:/etc/ssl/certs/coolify-ca.crt:ro" />
        </div>
      </SettingsSection>

      <SettingsSection id="server-ca-content-section" title="Certificate content" description="Review or replace the PEM certificate stored on this server.">
        {#snippet actions()}
          <div class="flex items-center gap-2">
            <button type="button" class="button" onclick={() => (show = !show)}>{show ? 'Hide certificate' : 'Show certificate'}</button>
            {#if canEdit}
              <button type="button" class="button" onclick={() => (confirm = 'save')}>Save certificate</button>
              <button type="button" class="button" onclick={() => (confirm = 'regenerate')}>Regenerate</button>
            {/if}
          </div>
        {/snippet}
        {#if show}
          <FormTextarea id="certificateContent" rows={15} label="PEM certificate" placeholder="Paste or edit CA certificate content here…" disabled={!canEdit} bind:value={content} />
        {:else}
          <div class="flex min-h-72 flex-col items-center justify-center rounded-lg bg-neutral-100/70 px-6 text-center ring-1 ring-neutral-200 dark:bg-black/20 dark:ring-white/[0.08]">
            <Reicon name="keys" class="size-8 text-neutral-300 dark:text-fg-faint" />
            <p class="mt-3 text-sm font-medium text-neutral-950 dark:text-fg">Certificate hidden</p>
            <p class="mt-1 text-xs text-neutral-500 dark:text-fg-dim">Show the certificate to review or edit its contents.</p>
          </div>
        {/if}
      </SettingsSection>
    </div>
  {/snippet}
</ServerFrame>

{#if confirm === 'save'}
  <ConfirmModal
    title="Confirm changing of CA Certificate?"
    actions={[
      'This overwrites /data/coolify/ssl/coolify-ca.crt with your custom certificate.',
      'Database certificates on this server will be regenerated and signed with the custom CA.',
      'You must redeploy affected databases and resources.',
    ]}
    confirmWithText={steps.text}
    confirmationText="/data/coolify/ssl/coolify-ca.crt"
    shortConfirmationLabel="CA Certificate Path"
    step3ButtonText="Save Certificate"
    onconfirm={save}
    onclose={() => (confirm = null)}
  />
{:else if confirm === 'regenerate'}
  <ConfirmModal
    title="Confirm Regenerate Certificate?"
    actions={[
      'This replaces the current CA certificate with a newly generated certificate.',
      'Database certificates on this server will be regenerated and signed with the new CA.',
      'You must redeploy affected databases and resources.',
    ]}
    confirmWithText={steps.text}
    confirmationText="/data/coolify/ssl/coolify-ca.crt"
    shortConfirmationLabel="CA Certificate Path"
    step3ButtonText="Regenerate Certificate"
    onconfirm={regenerate}
    onclose={() => (confirm = null)}
  />
{/if}
