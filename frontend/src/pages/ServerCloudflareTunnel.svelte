<script lang="ts">
  // Port of upstream livewire/server/cloudflare-tunnel.blade.php.
  import Callout from '../components/Callout.svelte';
  import ConfirmModal from '../components/ConfirmModal.svelte';
  import FormInput from '../components/FormInput.svelte';
  import Listbox from '../components/Listbox.svelte';
  import Reicon from '../components/Reicon.svelte';
  import ServerActivityDialog from '../components/ServerActivityDialog.svelte';
  import ServerFrame from '../components/ServerFrame.svelte';
  import SettingsSection from '../components/SettingsSection.svelte';
  import StatusBadge from '../components/StatusBadge.svelte';
  import { ApiError } from '../lib/api';
  import { link, router } from '../lib/router.svelte';
  import { get, post, failure } from '../lib/serverApi';
  import { session } from '../lib/session.svelte';
  import { notify } from '../lib/toast.svelte';

  let { uuid }: { uuid: string } = $props();

  type Data = {
    is_localhost: boolean;
    enabled: boolean;
    managed_tunnel: string | null;
    has_previous_ip: boolean;
    functional: boolean;
    port: number;
    has_cloudflare_tokens: boolean;
    tunnel_tokens: { id: number; name: string }[];
    confirm: { text: boolean; password: boolean };
  };

  let data = $state<Data | null>(null);
  let modal = $state<'manual' | 'disable' | null>(null);
  let dialog = $state<{ title: string; activity: number } | null>(null);
  let collapsed = $state(false);
  let error = $state<ApiError | null>(null);
  let tokenId = $state('');
  let apiDomain = $state('');
  let tunnelToken = $state('');
  let sshDomain = $state('');

  const url = $derived(`/server-page/${encodeURIComponent(uuid)}/cloudflare-tunnel`);
  const canEdit = $derived(session.isAdmin);
  const tokenOptions = $derived((data?.tunnel_tokens ?? []).map((t) => ({ value: String(t.id), label: t.name })));

  async function load() {
    const d = await get<Data>(url);
    if (d.is_localhost) {
      router.go(`/server/${uuid}`);
      return;
    }
    data = d;
  }
  /** The toast of the action, then a reload. */
  async function run(path: string) {
    try {
      const r = await post<{ message?: string; warning?: string; warnings?: string[] }>(`${url}/${path}`);
      for (const w of r.warnings ?? []) notify.warning(w);
      if (r.message) notify.success(r.message);
      if (r.warning) notify.warning(r.warning);
      await load();
    } catch (e) {
      failure(e);
    }
  }
  async function updateTunnel() {
    try {
      const r = await post<{ activity_id: number }>(`${url}/update`);
      dialog = { title: 'Cloudflare Tunnel Update Logs', activity: r.activity_id };
    } catch (e) {
      failure(e);
    }
  }
  interface DnsConflict {
    record_id: string;
    current: string;
    hostname: string;
    token_id: number;
  }
  let dnsConflict = $state<DnsConflict | null>(null);
  let confirmReplace = $state(false);
  // `startApiSetup`: the logs of `ConfigureCloudflared`, or the conflict to confirm.
  async function startApiSetup(body: Record<string, unknown>) {
    error = null;
    dnsConflict = null;
    try {
      const r = await post<{ activity_id?: number; dns_conflict?: DnsConflict }>(`${url}/api`, body);
      if (r.dns_conflict) {
        dnsConflict = r.dns_conflict;
      } else if (r.activity_id) {
        dialog = { title: 'Cloudflare Tunnel Configuration Logs', activity: r.activity_id };
      }
    } catch (e) {
      error = failure(e);
    }
  }
  const apiSetup = () => startApiSetup({ cloudflare_integration_token_id: tokenId || null, api_ssh_domain: apiDomain });
  async function replaceDnsAndConfigure() {
    const c = dnsConflict;
    confirmReplace = false;
    if (!c) return;
    await startApiSetup({
      cloudflare_integration_token_id: c.token_id,
      api_ssh_domain: c.hostname,
      confirmed_conflict: { record_id: c.record_id, current: c.current },
    });
  }
  async function automated() {
    try {
      const r = await post<{ activity_id: number }>(`${url}/automated`, { cloudflare_token: tunnelToken, ssh_domain: sshDomain });
      dialog = { title: 'Cloudflare Tunnel Configuration Logs', activity: r.activity_id };
    } catch (e) {
      failure(e);
    }
  }
</script>

{#snippet actions()}
  {#if data}
    {#if data.enabled && canEdit}
      <button type="button" class="button" onclick={updateTunnel}>Update Cloudflare Tunnel</button>
    {/if}
    {#if !data.enabled}
      <a class="button" href="https://coolify.io/docs/knowledge-base/cloudflare/tunnels/server-ssh" target="_blank">
        Documentation
        <svg class="inline-flex w-3 h-3 dark:text-neutral-400 text-black" fill="none" stroke="currentColor" viewBox="0 0 24 24">
          <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M10 6H6a2 2 0 00-2 2v10a2 2 0 002 2h10a2 2 0 002-2v-4M14 4h6m0 0v6m0-6L10 14" />
        </svg>
      </a>
    {/if}
    {#if !data.enabled && data.functional && canEdit}
      <button type="button" class="button flex gap-2" onclick={() => (modal = 'manual')}>I configured the tunnel manually</button>
    {/if}
    {#if data.enabled}
      <StatusBadge label="Enabled" type="success" />
    {/if}
  {/if}
{/snippet}

<ServerFrame {uuid} active="cloudflare-tunnel" title="Cloudflare Tunnel" onloaded={load}>
  {#snippet children()}
    {#if dialog}
      <ServerActivityDialog {uuid} activityId={dialog.activity} title={dialog.title} onclose={() => ((dialog = null), load())} />
    {/if}
    {#if data}
      <div class="application-settings-form flex w-full flex-col gap-6">
        <SettingsSection id="server-cloudflare-overview-section" title="Cloudflare Tunnel" description="Proxy SSH traffic through Cloudflare so the server SSH port can remain closed." {actions}>
          {#if data.enabled}
            {#if data.managed_tunnel}
              <p class="mb-4 text-sm leading-6 text-neutral-600 dark:text-fg-dim">
                coolify-rs manages this tunnel through the Cloudflare API (tunnel
                <code>{data.managed_tunnel}</code>).
              </p>
            {/if}
            <Callout type="warning" title="Disabling the tunnel can interrupt server access">The server IP must be restored to its direct address after disabling the tunnel.</Callout>
            {#if canEdit}
              <div class="mt-4">
                <button type="button" class="button button-error" onclick={() => (modal = 'disable')}>Disable Cloudflare Tunnel</button>
              </div>
            {/if}
          {:else if !data.functional}
            <Callout type="info" title="Validate the server for automated setup">
              Automated configuration requires a validated server, a Cloudflare token, and an SSH domain. You can also
              <button type="button" onclick={() => run('manual')} class="font-medium underline">mark a manual configuration as complete</button>.
            </Callout>
          {:else if !canEdit}
            <Callout type="danger" title="Insufficient permissions">You do not have permission to configure Cloudflare Tunnel for this server.</Callout>
          {:else}
            {#if data.tunnel_tokens.length === 0}
              <Callout type="info" title="No Cloudflare token can manage tunnels">
                {data.has_cloudflare_tokens ? 'Enable the Cloudflare Tunnel capability on a Cloudflare token' : 'Add a Cloudflare token with the DNS and Cloudflare Tunnel capabilities'}
                in
                <a href="/security/integration-tokens" class="font-medium underline" use:link>Integration tokens</a>. The token needs Account - Cloudflare Tunnel - Edit, Zone - DNS - Edit, and Zone - Zone - Read.
              </Callout>
              <div class="mt-4">
                <a class="button button-highlighted" href="/security/integration-tokens" use:link>
                  <Reicon name="plus" class="size-3.5" />
                  Add Cloudflare token
                </a>
              </div>
            {:else}
              <form
                onsubmit={(e) => {
                  e.preventDefault();
                  apiSetup();
                }}
              >
                <div class="grid gap-4 lg:grid-cols-2">
                  <Listbox id="cloudflare_integration_token_id" label="Cloudflare token" required placeholder="Select a token" options={tokenOptions} bind:value={tokenId} error={error?.field('cloudflare_integration_token_id')} />
                  <FormInput
                    id="api_ssh_domain"
                    label="SSH hostname"
                    required
                    placeholder="ssh.example.com"
                    helper="A hostname in a zone of the selected token. Cloudflare routes it to SSH port {data.port} of this server."
                    bind:value={apiDomain}
                    error={error?.field('api_ssh_domain')}
                  />
                </div>
                <div class="mt-4 flex justify-end">
                  <button type="submit" class="button" {...{ ishighlighted: '' }}>Set up tunnel</button>
                </div>
                {#if dnsConflict}
                  <div class="mt-4">
                    <Callout type="warning" title="A DNS record already exists">
                      {dnsConflict.hostname} already points to {dnsConflict.current}. Replace it with the tunnel record to continue.
                    </Callout>
                    <div class="mt-4">
                      <button type="button" class="button" onclick={() => (confirmReplace = true)}>Replace DNS record</button>
                    </div>
                  </div>
                {/if}
              </form>
            {/if}

            <div class="mt-4 border-t border-neutral-200 pt-4 dark:border-white/[0.08]">
              <div class="flex flex-col gap-4">
                <button type="button" onclick={() => (collapsed = !collapsed)} class="flex items-center gap-2 text-left text-sm font-medium hover:underline" aria-expanded={collapsed}>
                  <svg class="size-4 transition-transform {collapsed ? 'rotate-90' : ''}" viewBox="0 0 20 20" fill="currentColor" aria-hidden="true">
                    <path fill-rule="evenodd" d="M7.21 14.77a.75.75 0 0 1 .02-1.06L11.168 10 7.23 6.29a.75.75 0 1 1 1.04-1.08l4.5 4.25a.75.75 0 0 1 0 1.08l-4.5 4.25a.75.75 0 0 1-1.06-.02Z" clip-rule="evenodd" />
                  </svg>
                  Use an existing tunnel with its tunnel token
                </button>
                {#if collapsed}
                  <div class="rounded-lg border border-neutral-200 p-4 dark:border-coolgray-400">
                    <p class="text-sm leading-6 text-neutral-600 dark:text-fg-dim">For a tunnel you created in the Cloudflare dashboard. coolify-rs does not need a Cloudflare API token for this.</p>
                    <form
                      class="mt-4"
                      onsubmit={(e) => {
                        e.preventDefault();
                        automated();
                      }}
                    >
                      <div class="grid gap-4 lg:grid-cols-2">
                        <FormInput id="cloudflare_token" required label="Tunnel token" type="password" helper="The token of the tunnel connector, from the Cloudflare dashboard." bind:value={tunnelToken} />
                        <FormInput id="ssh_domain" label="SSH domain" required helper="The public hostname of the tunnel that routes to SSH on this server, without a protocol." bind:value={sshDomain} />
                      </div>
                      <div class="mt-4 flex justify-end">
                        <button type="submit" class="button">Configure tunnel</button>
                      </div>
                    </form>
                  </div>
                {/if}
              </div>
            </div>
          {/if}
        </SettingsSection>
      </div>

      {#if modal === 'manual'}
        <ConfirmModal
          title="Confirm manual Cloudflare Tunnel configuration"
          actions={['Cloudflare and cloudflared have already been configured.', 'An incomplete setup can make the server unreachable.']}
          confirmationText="I manually configured Cloudflare Tunnel"
          confirmationLabel="Type the confirmation text to continue."
          shortConfirmationLabel="Confirmation text"
          confirmWithText={data.confirm.text}
          confirmWithPassword={data.confirm.password}
          onconfirm={() => run('manual')}
          onclose={() => (modal = null)}
        />
      {:else if modal === 'disable'}
        <ConfirmModal
          title="Disable Cloudflare Tunnel?"
          actions={[
            'Cloudflare Tunnel will be disabled for this server.',
            ...(data.managed_tunnel ? ['The Cloudflare tunnel and the DNS record that coolify-rs created will be deleted.'] : []),
            ...(data.has_previous_ip ? ['The server IP address will be restored to its previous value.'] : ['You must manually restore the direct server IP address.', 'The server may become inaccessible until the IP is corrected.']),
          ]}
          confirmationText="DISABLE CLOUDFLARE TUNNEL"
          confirmationLabel="Type the confirmation text to disable Cloudflare Tunnel."
          shortConfirmationLabel="Confirmation text"
          confirmWithText={data.confirm.text}
          confirmWithPassword={data.confirm.password}
          onconfirm={() => run('disable')}
          onclose={() => (modal = null)}
        />
      {/if}
    {/if}
  {/snippet}
</ServerFrame>

{#if confirmReplace && dnsConflict}
  <ConfirmModal
    title="Replace DNS record?"
    actions={[`The DNS record of ${dnsConflict.hostname} will point to the Cloudflare tunnel.`, 'Traffic that uses the current record value will stop.']}
    confirmWithText={false}
    confirmWithPassword={false}
    onconfirm={replaceDnsAndConfigure}
    onclose={() => (confirmReplace = false)}
  />
{/if}
