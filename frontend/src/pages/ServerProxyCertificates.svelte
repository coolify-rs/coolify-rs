<script lang="ts">
  // Port of upstream livewire/server/proxy/certificates-show.blade.php and certificates.blade.php.
  // Deleting certificates and the acme.json backups are not ported.
  import Empty from '../components/Empty.svelte';
  import ServerFrame from '../components/ServerFrame.svelte';
  import SettingsSection from '../components/SettingsSection.svelte';
  import { get, failure } from '../lib/serverApi';

  let { uuid }: { uuid: string } = $props();
  type Cert = { id: string; resolver: string; main_domain: string; sans: string[]; store: string | null; expires_at: string | null };
  let certs = $state<Cert[]>([]);
  let loaded = $state(false);

  async function load() {
    loaded = false;
    try {
      certs = (await get<{ certificates: Cert[] }>(`/server-page/${encodeURIComponent(uuid)}/proxy/certificates`)).certificates;
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
            description="Review certificates stored in Traefik's acme.json file. Deleting an entry removes it from local ACME storage. It does not revoke the certificate at the certificate authority. If a route still uses the domain, Traefik requests a new certificate after the restart."
          >
            {#snippet actions()}
              <button type="button" class="button" disabled={!loaded} onclick={load}>Refresh</button>
            {/snippet}
            {#if loaded && certs.length === 0}
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
                        <th><span class="sr-only">Actions</span></th>
                      </tr>
                    </thead>
                    <tbody>
                      {#each certs as c (c.id)}
                        <tr>
                          <td class="font-medium text-neutral-950 dark:text-fg">{c.main_domain}</td>
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
                          <td class="text-right"></td>
                        </tr>
                      {/each}
                    </tbody>
                  </table>
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
