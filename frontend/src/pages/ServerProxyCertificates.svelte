<script lang="ts">
  import Reicon from '../components/Reicon.svelte';
  import RowActions from '../components/RowActions.svelte';
  import RowAction from '../components/RowAction.svelte';
  // Port of upstream livewire/server/proxy/certificates-show.blade.php and certificates.blade.php.
  import ConfirmModal from '../components/ConfirmModal.svelte';
  import Empty from '../components/Empty.svelte';
  import SkeletonTable from '../components/SkeletonTable.svelte';
  import StatusBadge from '../components/StatusBadge.svelte';
  import ServerFrame from '../components/ServerFrame.svelte';
  import SettingsSection from '../components/SettingsSection.svelte';
  import { get, post, failure } from '../lib/serverApi';
  import { session } from '../lib/session.svelte';
  import { notify } from '../lib/toast.svelte';

  let { uuid }: { uuid: string } = $props();
  const WARNING_DELETE = 'The proxy keeps using the certificate until you restart it. If a route still uses the domain, Traefik requests a new certificate after the restart. You can restore the backup below to undo this.';
  const WARNING_RESTORE = 'Until the proxy restarts, it keeps its loaded certificates and can write them back to acme.json. Certificates issued after this backup are removed from acme.json.';
  const RESTART_LABEL = 'Restart the proxy now. Sites on this server are unavailable for a few seconds.';
  /** `in_use` (v4.7.5): false when no route and no resource on the server uses a domain of the certificate, null when it could not be checked. */
  type Cert = { id: string; resolver: string; main_domain: string; sans: string[]; store: string | null; expires_at: string | null; in_use?: boolean | null };
  type Backup = { name: string; created_at: string; size: number };
  let certs = $state<Cert[]>([]);
  let backups = $state<Backup[]>([]);
  let keep = $state(10);
  let deleting = $state<Cert | null>(null);
  let deletingUnused = $state(false);
  const unused = $derived(certs.filter((c) => c.in_use === false));
  let restoring = $state<Backup | null>(null);
  let deletingBackup = $state<Backup | null>(null);
  const base = $derived(`/server-page/${encodeURIComponent(uuid)}/proxy/certificates`);
  const canEdit = $derived(session.isAdmin);

  /** `formatBytes`. */
  function formatBytes(bytes: number): string {
    if (bytes <= 0) return '0 B';
    const units = ['B', 'KB', 'MB', 'GB', 'TB'];
    const i = Math.min(Math.floor(Math.log(bytes) / Math.log(1024)), units.length - 1);
    return `${Math.round((bytes / 1024 ** i) * 100) / 100} ${units[i]}`;
  }
  async function act(path: string, body: unknown) {
    try {
      const r = await post<{ message: string }>(`${base}/${path}`, body);
      notify.success(r.message);
      await load();
    } catch (e) {
      failure(e);
    }
  }
  /** `deleteUnusedTraefikCertificates`. */
  async function deleteUnused() {
    try {
      const r = await post<{ message?: string; info?: string }>(`${base}/delete-unused`, {});
      if (r.message) notify.success(r.message);
      else if (r.info) notify.info(r.info);
      await load();
    } catch (e) {
      failure(e);
    }
  }
  let loaded = $state(false);

  async function load() {
    loaded = false;
    try {
      const r = await get<{ certificates: Cert[]; backups: Backup[]; keep: number }>(base);
      certs = r.certificates;
      backups = r.backups;
      keep = r.keep;
    } catch (e) {
      failure(e);
    } finally {
      loaded = true;
    }
  }
</script>

<ServerFrame {uuid} active="proxy" sub="certificates" title="TLS Certificates" onloaded={(f) => f.functional && f.proxy_type === 'TRAEFIK' && load()}>
  {#snippet children(f)}
    {#if f.functional && f.proxy_type === 'TRAEFIK'}
      <div class="w-full">
        <div class="application-settings-form flex w-full flex-col gap-6">
          <SettingsSection
            id="server-proxy-certificates-section"
            title="TLS certificates"
            description="Review certificates stored in Traefik's acme.json file. Traefik renews every certificate in this file, also when no route uses its domain any more. Deleting an entry removes it from local ACME storage. It does not revoke the certificate at the certificate authority. If a route still uses the domain, Traefik requests a new certificate after the restart."
          >
            {#snippet actions()}
              {#if unused.length > 0 && canEdit}
                <button type="button" class="button button-error" onclick={() => (deletingUnused = true)}>Delete unused ({unused.length})</button>
              {/if}
              <button type="button" class="button" disabled={!loaded} onclick={load}>Refresh</button>
            {/snippet}
            {#if !loaded}
              <div class="overflow-hidden rounded-lg ring-1 ring-neutral-200 dark:ring-white/[0.08]"><SkeletonTable flush rows={4} /></div>
            {:else if certs.length === 0}
              <Empty size="sm" title="No TLS certificates found" description="Traefik's ACME storage does not contain certificate entries." iconName="shield-star" />
            {:else if certs.length > 0}
              <div class="overflow-hidden rounded-lg ring-1 ring-neutral-200 dark:ring-white/[0.08]">
                <div class="overflow-x-auto">
                  <table class="w-full min-w-3xl">
                    <thead>
                      <tr>
                        <th>Domain</th>
                        <th>Resolver</th>
                        <th>Alternative names</th>
                        <th>Expires</th>
                        <th class="text-right">Actions</th>
                      </tr>
                    </thead>
                    <tbody>
                      {#each certs as c (c.id)}
                        <tr>
                          <td class="font-medium text-neutral-950 dark:text-fg">
                            <div class="flex flex-col items-start gap-1">
                              {c.main_domain}
                              {#if c.in_use === false}
                                <StatusBadge
                                  label="Not used"
                                  type="warning"
                                  size="sm"
                                  title="No route and no resource on this server uses this domain. Traefik renews the certificate until you delete it."
                                />
                              {/if}
                            </div>
                          </td>
                          <td>
                            <span class="font-mono text-xs">{c.resolver}</span>
                            {#if c.store}<span class="text-xs text-neutral-500 dark:text-fg-dim">({c.store})</span>{/if}
                          </td>
                          <td>
                            {#if c.sans.length > 0}
                              <div class="flex max-w-md flex-wrap gap-1">
                                {#each c.sans as d (d)}
                                  <span class="rounded bg-neutral-100 px-1.5 py-0.5 font-mono text-xs text-neutral-700 dark:bg-white/[0.06] dark:text-fg-dim">{d}</span>
                                {/each}
                              </div>
                            {:else}
                              <span class="text-neutral-500 dark:text-fg-dim">None</span>
                            {/if}
                          </td>
                          <td>{c.expires_at ?? 'Unknown'}</td>
                          <td>
                            {#if canEdit}
                              <div class="flex justify-end">
                                <button
                                  type="button"
                                  class="icon-button shrink-0 text-red-500 hover:text-red-600 dark:text-red-400 dark:hover:text-red-300"
                                  title="Delete certificate"
                                  aria-label="Delete certificate for {c.main_domain}"
                                  onclick={() => (deleting = c)}
                                >
                                  <Reicon name="trash" class="size-3.5" />
                                </button>
                              </div>
                            {/if}
                          </td>
                        </tr>
                      {/each}
                    </tbody>
                  </table>
                </div>
              </div>
            {/if}
            {#if canEdit && backups.length > 0}
              <div class="mt-6 flex flex-col gap-2">
                <div>
                  <h4 class="text-sm font-medium text-neutral-950 dark:text-fg">acme.json backups</h4>
                  <p class="mt-1 text-xs text-neutral-500 dark:text-fg-dim">coolify-rs saves a copy of acme.json before it changes the file and keeps the last {keep} copies.</p>
                </div>
                <div class="overflow-hidden rounded-lg ring-1 ring-neutral-200 dark:ring-white/[0.08]">
                  <div class="overflow-x-auto">
                    <table class="w-full min-w-2xl">
                      <thead>
                        <tr>
                          <th>Backup</th>
                          <th>Created</th>
                          <th>Size</th>
                          <th class="text-right">Actions</th>
                        </tr>
                      </thead>
                      <tbody>
                        {#each backups as backup (backup.name)}
                          <tr>
                            <td class="font-mono text-xs text-neutral-950 dark:text-fg">{backup.name}</td>
                            <td>{backup.created_at}</td>
                            <td>{formatBytes(backup.size)}</td>
                            <td>
                              <div class="flex justify-end">
                                <RowActions label="Backup actions">
                                  <RowAction onclick={() => (restoring = backup)}>Restore</RowAction>
                                  {#snippet danger()}
                                    <RowAction onclick={() => (deletingBackup = backup)}>Delete</RowAction>
                                  {/snippet}
                                </RowActions>
                              </div>
                            </td>
                          </tr>
                        {/each}
                      </tbody>
                    </table>
                  </div>
                </div>
              </div>
            {/if}
          </SettingsSection>
        </div>
      </div>
    {:else}
      <div class="application-settings-form w-full">
        <SettingsSection title="TLS certificates" description="Review certificates stored by Traefik for this server.">
          <Empty
            size="sm"
            title={f.functional ? 'Traefik required' : 'Server validation required'}
            description={f.functional ? 'TLS certificate management is available for the Traefik proxy.' : 'Validate this server before viewing its TLS certificates.'}
            iconName="servers"
          />
        </SettingsSection>
      </div>
    {/if}
  {/snippet}
</ServerFrame>

{#if deleting}
  {@const c = deleting}
  <ConfirmModal
    title="Delete TLS Certificate?"
    actions={['Save the current acme.json as a backup that you can restore below.', `Delete the certificate for ${c.main_domain} from acme.json.`]}
    warningMessage={WARNING_DELETE}
    confirmationText={c.main_domain}
    confirmationLabel="Confirm by entering the domain"
    shortConfirmationLabel="Domain"
    step2ButtonText="Delete Certificate"
    onconfirm={() => act('delete', { id: c.id })}
    onclose={() => (deleting = null)}
  />
{/if}
{#if deletingUnused}
  <ConfirmModal
    title="Delete Unused TLS Certificates?"
    actions={[
      'Save the current acme.json as a backup that you can restore below.',
      `Delete ${unused.length} certificates that no route and no resource on this server uses: ${unused.map((c) => c.main_domain).join(', ')}.`,
    ]}
    warningMessage="The proxy keeps using the certificates until you restart it. If a route still uses a domain, Traefik requests a new certificate after the restart. You can restore the backup below to undo this."
    confirmWithText={false}
    confirmWithPassword={false}
    step2ButtonText="Delete Certificates"
    onconfirm={deleteUnused}
    onclose={() => (deletingUnused = false)}
  />
{/if}
{#if restoring}
  {@const b = restoring}
  <ConfirmModal
    title="Restore acme.json Backup?"
    checkboxes={[{ id: 'restartProxyAfterAcmeRestore', label: RESTART_LABEL, checked: true }]}
    actions={['Save the current acme.json as a new backup.', `Replace acme.json with the backup from ${b.created_at}.`]}
    warningMessage={WARNING_RESTORE}
    confirmWithText={false}
    step2ButtonText="Restore Backup"
    onconfirm={(_, selected) => act('backups/restore', { name: b.name, restart: selected.includes('restartProxyAfterAcmeRestore') })}
    onclose={() => (restoring = null)}
  />
{/if}
{#if deletingBackup}
  {@const b = deletingBackup}
  <ConfirmModal
    title="Delete acme.json Backup?"
    actions={[`Delete the backup ${b.name}.`]}
    confirmWithText={false}
    step2ButtonText="Delete Backup"
    onconfirm={() => act('backups/delete', { name: b.name })}
    onclose={() => (deletingBackup = null)}
  />
{/if}
