<script lang="ts">
  import Forbidden from '../components/Forbidden.svelte';
  // Port of upstream livewire/server/sentinel/show.blade.php and livewire/server/sentinel.blade.php.
  import Reicon from '../components/Reicon.svelte';
  import Callout from '../components/Callout.svelte';
  import ConfirmModal from '../components/ConfirmModal.svelte';
  import Empty from '../components/Empty.svelte';
  import FormInput from '../components/FormInput.svelte';
  import ServerFrame from '../components/ServerFrame.svelte';
  import SettingsSection from '../components/SettingsSection.svelte';
  import StatusBadge from '../components/StatusBadge.svelte';
  import UnsavedBar from '../components/UnsavedBar.svelte';
  import { ApiError } from '../lib/api';
  import type { ServerFacts } from '../lib/api';
  import { get, post, failure } from '../lib/serverApi';
  import { link } from '../lib/router.svelte';
  import { session } from '../lib/session.svelte';
  import { notify } from '../lib/toast.svelte';

  let { uuid }: { uuid: string } = $props();
  type Data = { enabled: boolean; status: string; push_problem: string | null; url: string | null; token: string | null; is_localhost: boolean };
  let data = $state<Data | null>(null);
  let url = $state('');
  let token = $state('');
  let saved = $state({ url: '', token: '' });
  let saving = $state(false);
  let error = $state<ApiError | null>(null);
  let restoring = $state(false);
  const base = $derived(`/server-page/${encodeURIComponent(uuid)}/sentinel`);
  const canEdit = $derived(session.isAdmin);
  /**
   * `isPushUrlInsecure()` (v4.7.5): Sentinel sends its token in the Authorization header of every
   * push, readable on the network over plain HTTP. Only the known local internal endpoint stays on
   * the coolify Docker network.
   */
  const pushUrlInsecure = $derived(!(data?.is_localhost && url === 'http://coolify:8080') && url.toLowerCase().startsWith('http://'));
  const dirty = $derived(url !== saved.url || token !== saved.token);
  let poll: ReturnType<typeof setInterval> | undefined;

  const statusLabel = $derived(({ restarting: 'Restarting', waiting: 'Waiting for first report', in_sync: 'In sync' } as Record<string, string>)[data?.status ?? ''] ?? 'Out of sync');
  const statusType = $derived(data?.status === 'in_sync' ? 'success' : data?.status === 'out_of_sync' ? 'warning' : 'neutral');
  /** `escapeshellarg`. */
  const shellArg = (v: string) => `'${v.replace(/'/g, `'\\''`)}'`;

  async function load(f?: ServerFacts, full = true) {
    if (f && !f.functional) return;
    const d = await get<Data>(base);
    data = d;
    if (full) {
      url = saved.url = d.url ?? '';
      token = saved.token = d.token ?? '';
    }
    clearInterval(poll);
    // `wire:poll.10s="refreshSentinelStatus"`.
    poll = setInterval(() => load(undefined, false), 10000);
  }
  $effect(() => () => clearInterval(poll));

  async function call(path: string, success: string, kind: 'success' | 'info' = 'success') {
    try {
      await post(`${base}/${path}`);
      if (kind === 'info') notify.info('Info', success);
      else notify.success(success);
      await load();
    } catch (e) {
      failure(e);
    }
  }
  async function submit() {
    saving = true;
    error = null;
    try {
      await post(base, { sentinel_token: token, sentinel_custom_url: url || null });
      saved = { url, token };
      notify.success('Sentinel settings updated. Restarting Sentinel.');
      await load();
    } catch (e) {
      error = failure(e);
    } finally {
      saving = false;
    }
  }
</script>

<ServerFrame {uuid} active="sentinel" sub="configuration" title="Sentinel Configuration" onloaded={load}>
  {#snippet children(f)}
    {#if !f.functional}
      <div class="application-settings-form w-full">
        <SettingsSection title="Sentinel" description="Monitor server and container health while collecting metrics.">
          <Empty size="sm" title="Server validation required" description="Validate this server before enabling Sentinel." iconName="dashboard" />
        </SettingsSection>
      </div>
    {:else if data}
      <div class="w-full">
        <div class="application-settings-form flex w-full flex-col gap-6">
          <form
            class="contents"
            onsubmit={(e) => {
              e.preventDefault();
              submit();
            }}
          >
            <UnsavedBar {dirty} {saving} onsave={submit} onreset={() => ((url = saved.url), (token = saved.token), (error = null))} />
            <SettingsSection id="server-sentinel-overview-section" title="Sentinel" description="Monitor server and container health while collecting historical metrics.">
              {#snippet actions()}
                <div class="flex items-center gap-2">
                  <StatusBadge label={statusLabel} type={statusType} />
                  <button type="button" class="button" disabled={!canEdit} onclick={() => call('restart', 'Restarting Sentinel.', 'info')}>
                    <Reicon name="refresh" class="size-3.5" />
                    {data?.status === 'in_sync' ? 'Restart' : 'Sync'}
                  </button>
                </div>
              {/snippet}
              {#if data.status === 'out_of_sync'}
                <Callout type="warning" title="Sentinel is out of sync">
                  <div class="space-y-3">
                    <p>Sentinel has not reported within the expected interval. Check these items before syncing again:</p>
                    {#if data.push_problem}
                      <p><span class="font-medium">Last error:</span> <code class="break-all">{data.push_problem}</code></p>
                    {/if}
                    <ul class="list-disc space-y-1 pl-4">
                      <li>Confirm that the <code>coolify-sentinel</code> container is running.</li>
                      <li>
                        <a class="font-medium underline underline-offset-2" href="/server/{uuid}/sentinel/logs" use:link>Open Sentinel logs</a>
                        and review recent connection or push errors.
                      </li>
                      <li>Confirm that the coolify-rs URL and Sentinel token match this configuration.</li>
                    </ul>
                    {#if data.is_localhost}
                      <p>Sync Sentinel to recreate it on the coolify-rs Docker network.</p>
                    {:else}
                      <div class="space-y-2">
                        <p>The remote server needs outbound access to this coolify-rs URL. Sentinel reporting does not require an inbound listening port.</p>
                        {#if url}
                          <p>From the remote server, test <code class="break-all">curl -fsS {shellArg(`${url.replace(/\/+$/, '')}/api/health`)}</code>.</p>
                        {:else}
                          <p>Set a reachable coolify-rs URL before syncing Sentinel.</p>
                        {/if}
                        <p>Check DNS, TLS certificates, outbound firewall rules, and proxy settings.</p>
                      </div>
                    {/if}
                  </div>
                </Callout>
              {:else if data.status === 'in_sync'}
                <p class="text-sm text-neutral-500 dark:text-fg-dim">Sentinel is connected and reporting server health to this coolify-rs instance.</p>
              {:else if data.status === 'restarting'}
                <p class="text-sm text-neutral-500 dark:text-fg-dim">Sentinel is restarting.</p>
              {:else}
                <p class="text-sm text-neutral-500 dark:text-fg-dim">Sentinel started and is waiting for its first authenticated report.</p>
              {/if}
            </SettingsSection>

            {#if data.enabled}
              <SettingsSection id="server-sentinel-connection-section" title="Connection" description="Configure how Sentinel authenticates with and reports to coolify-rs.">
                {#snippet actions()}
                  <div class="flex items-center gap-2">
                    {#if canEdit}
                      <button type="button" class="button" onclick={() => (restoring = true)}>Restore defaults</button>
                    {/if}
                    <button type="button" class="button" disabled={!canEdit} onclick={() => call('regenerate', 'Token regenerated. Restarting Sentinel.')}>Regenerate token</button>
                  </div>
                {/snippet}
                <div class="flex flex-col gap-4">
                  {#if pushUrlInsecure}
                    <Callout type="warning" title="Sentinel reports over plain HTTP">
                      Sentinel sends its token with every report. Over HTTP, the token and the server data are not encrypted on the network. Set an HTTPS coolify-rs URL.
                    </Callout>
                  {/if}
                  <div class="grid gap-4 lg:grid-cols-2">
                    <FormInput disabled={!canEdit} id="sentinelCustomUrl" required label="coolify-rs URL" helper="Public URL used by Sentinel to reach this coolify-rs instance." bind:value={url} error={error?.field('sentinel_custom_url')} />
                    <FormInput disabled={!canEdit} type="password" id="sentinelToken" label="Sentinel token" required helper="Authentication token used by Sentinel." bind:value={token} error={error?.field('sentinel_token')} />
                  </div>
                </div>
              </SettingsSection>
            {/if}
          </form>
        </div>
      </div>
    {/if}
  {/snippet}
</ServerFrame>

{#if restoring}
  <ConfirmModal
    title="Restore default Sentinel configuration?"
    actions={[
      'Restore the generated coolify-rs URL and default collection settings.',
      'Clear debug logging and the development image override.',
      'The Sentinel token and metrics setting will be preserved.',
      'Restart Sentinel to apply the restored configuration.',
    ]}
    warningMessage="Your custom Sentinel configuration will be replaced with coolify-rs defaults."
    confirmWithText={false}
    step2ButtonText="Restore defaults"
    onconfirm={() => call('restore', 'Default Sentinel configuration restored. Restarting Sentinel.')}
    onclose={() => (restoring = false)}
  />
{/if}

{#if !session.isAdmin}
  <Forbidden />
{/if}
