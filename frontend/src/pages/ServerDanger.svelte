<script lang="ts">
  // Port of upstream livewire/server/delete.blade.php.
  import Empty from '../components/Empty.svelte';
  import DangerZone from '../components/DangerZone.svelte';
  import ConfirmModal from '../components/ConfirmModal.svelte';
  import ServerLayout from '../components/ServerLayout.svelte';
  import SettingsSection from '../components/SettingsSection.svelte';
  import { serversUi, ApiError, type ServerFacts, type ServersPageData } from '../lib/api';
  import { loadServerContext } from '../lib/serverPage.svelte';
  import { router } from '../lib/router.svelte';

  let { uuid }: { uuid: string } = $props();

  let facts = $state<ServerFacts | null>(null);
  let confirm = $state<ServersPageData['confirm']>({ text: true, password: true });
  let missing = $state(false);
  let deleting = $state(false);

  $effect(() => {
    loadServerContext(uuid, 'danger').then((ctx) => {
      if (!ctx) {
        missing = true;
        return;
      }
      facts = ctx.facts;
      confirm = ctx.list.confirm;
      document.title = `${ctx.facts.name.slice(0, 10)} > Delete Server | coolify-rs`;
    });
  });

  // `Server\Delete::render`.
  const checkboxes = $derived.by(() => {
    if (!facts || facts.transferred) return [];
    const list: { id: string; label: string; defaultWarning: string }[] = [];
    const f = facts as ServerFacts & Record<string, unknown>;
    if (f.resources > 0) {
      list.push({
        id: 'force_delete_resources',
        label: `Delete all resources (${f.resources} total)`,
        defaultWarning: 'Server cannot be deleted while it has resources.',
      });
    }
    if (f.hetzner_server_id) {
      list.push({ id: 'delete_from_hetzner', label: 'Also delete server from Hetzner Cloud', defaultWarning: 'The actual server on Hetzner Cloud will NOT be deleted.' });
    }
    if (f.vultr_instance_id) {
      list.push({ id: 'delete_from_vultr', label: 'Also delete server from Vultr', defaultWarning: 'The actual server on Vultr will NOT be deleted.' });
    }
    if (f.digitalocean_droplet_id) {
      list.push({ id: 'delete_from_digitalocean', label: 'Also delete droplet from DigitalOcean', defaultWarning: 'The actual droplet on DigitalOcean will NOT be deleted.' });
    }
    if (f.hostinger_virtual_machine_id) {
      list.push({
        id: 'delete_from_hostinger',
        label: 'Also delete VPS from Hostinger (at the end of the paid period)',
        defaultWarning: 'The VPS on Hostinger will NOT be deleted, and Hostinger will keep billing for it.',
      });
    }
    return list;
  });

  async function remove(password: string, selected: string[]): Promise<string | void> {
    try {
      await serversUi.remove(uuid, password, selected);
      router.go('/servers');
    } catch (e) {
      if (e instanceof ApiError && e.message === 'The provided password is incorrect.') return e.message;
      throw e;
    }
  }
</script>

{#if missing}
  <div class="application-settings-form w-full">
    <Empty title="Server not found" description="It may have been deleted, or it belongs to another team." iconName="servers" size="lg" />
  </div>
{:else if facts}
  <ServerLayout name={facts.name}>
    <div class="application-settings-form w-full">
      {#if !facts.is_coolify_host}
        <div class="server-danger-section">
          <SettingsSection title="Delete server" description="Permanently remove this server and its configuration from Coolify.">
            <DangerZone title="This action cannot be undone">
              <p>
                The server will be removed from Coolify.
                {#if facts.transferred}
                  This server was transferred to another Coolify instance. Only the records on this instance are deleted, also the records of its resources. The
                  server, its containers, and its data stay as they are.
                {:else if facts.resources > 0}
                  It currently contains managed resources. Enable force deletion in the confirmation only if those resources should also be removed.
                {/if}
              </p>
              <p>Type the server name in the confirmation dialog to continue.</p>
              {#snippet action()}
                <div class="relative h-auto max-w-full inline-flex w-auto">
                  <button type="button" class="button button-error flex gap-2" onclick={() => (deleting = true)}>Delete server</button>
                </div>
              {/snippet}
            </DangerZone>
          </SettingsSection>
        </div>
      {/if}
    </div>
  </ServerLayout>
{/if}

{#if deleting && facts}
  <ConfirmModal
    title="Confirm Server Deletion?"
    actions={facts.transferred
      ? ['This server and the records of its resources will be permanently deleted from this Coolify instance. Nothing on the server changes.']
      : ['This server will be permanently deleted from Coolify.']}
    {checkboxes}
    confirmWithText={confirm.text}
    confirmWithPassword={confirm.password}
    confirmationText={facts.name}
    confirmationLabel="Please confirm by entering the Server Name below"
    shortConfirmationLabel="Server Name"
    onconfirm={remove}
    onclose={() => (deleting = false)}
  />
{/if}
