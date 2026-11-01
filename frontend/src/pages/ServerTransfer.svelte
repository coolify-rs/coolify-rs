<script lang="ts">
  // Port of upstream livewire/server/transfer.blade.php.
  import Callout from '../components/Callout.svelte';
  import ConfirmModal from '../components/ConfirmModal.svelte';
  import FormInput from '../components/FormInput.svelte';
  import ServerFrame from '../components/ServerFrame.svelte';
  import SettingsSection from '../components/SettingsSection.svelte';
  import StatusBadge from '../components/StatusBadge.svelte';
  import { ApiError } from '../lib/api';
  import { router } from '../lib/router.svelte';
  import { get, post, failure } from '../lib/serverApi';
  import { notify } from '../lib/toast.svelte';

  let { uuid }: { uuid: string } = $props();

  type LogEntry = { at: string; level: string; message: string };
  type Migration = {
    status?: string;
    phase?: string;
    target_url?: string;
    error?: string;
    log?: LogEntry[];
    warnings?: string[];
    checklist?: unknown[];
    target_server_url?: string;
  };
  type Page = {
    is_localhost: boolean;
    is_owner: boolean;
    is_transferred: boolean;
    export_id: string | null;
    owner_marker_conflict: { instance_url?: string } | null;
    transfer_back: { requested_at?: string; from?: string } | null;
    manager_url: string;
    this_instance_url: string;
    migration: Migration | null;
    target_server_url: string | null;
  };

  let data = $state<Page | null>(null);
  let targetUrl = $state('');
  let targetToken = $state('');
  let managerUrl = $state('');
  let managerToken = $state('');
  let thisInstanceUrl = $state('');
  let confirming = $state<'' | 'migrate' | 'back'>('');
  let busy = $state(false);
  let lastWarnings = $state<string[]>([]);

  const base = $derived(`/server-page/${encodeURIComponent(uuid)}/transfer`);
  const exportTooltip = $derived(data?.export_id ? `Export ID: ${data.export_id}` : undefined);
  const migration = $derived(data?.migration ?? null);
  const log = $derived(migration?.log ?? []);
  const checklist = $derived(
    migration?.status === 'succeeded' ? (migration.checklist ?? []).filter((s): s is string => typeof s === 'string') : [],
  );
  const managerHost = $derived.by(() => {
    try {
      return new URL(managerUrl).host;
    } catch {
      return 'an invalid URL';
    }
  });

  async function load(first = false) {
    try {
      const d = await get<Page>(base);
      data = d;
      if (first) {
        managerUrl = d.manager_url;
        thisInstanceUrl = d.this_instance_url;
      } else {
        lastWarnings = d.migration?.warnings ?? lastWarnings;
      }
    } catch (e) {
      failure(e);
      if (first) router.go('/servers');
    }
  }

  $effect(() => {
    load(true);
  });

  // `wire:poll.5s="refreshMigration"` while a transfer runs.
  $effect(() => {
    if (migration?.status !== 'running') return;
    const timer = setInterval(() => load(), 5000);
    return () => clearInterval(timer);
  });

  /** `Carbon::parse($at)->format($format)`. */
  function clock(at: string | undefined, seconds: boolean): string {
    const d = at ? new Date(at) : new Date();
    const part = (n: number) => String(n).padStart(2, '0');
    const base = `${part(d.getUTCHours())}:${part(d.getUTCMinutes())}`;
    return seconds ? `${base}:${part(d.getUTCSeconds())}` : base;
  }

  async function migrateServer(): Promise<string | void> {
    busy = true;
    try {
      await post(`${base}/migrate`, { target_url: targetUrl, target_token: targetToken });
      lastWarnings = [];
      notify.success('All checks passed. The transfer started.');
    } catch (e) {
      failure(e as ApiError);
    } finally {
      targetToken = '';
      busy = false;
      confirming = '';
      await load();
    }
  }

  async function transferBack(): Promise<string | void> {
    busy = true;
    try {
      await post(`${base}/back`, { manager_url: managerUrl, manager_token: managerToken, this_instance_url: thisInstanceUrl });
      notify.success('The other instance started the transfer back.');
    } catch (e) {
      failure(e as ApiError);
    } finally {
      managerToken = '';
      busy = false;
      confirming = '';
      await load();
    }
  }
</script>

<ServerFrame {uuid} active="transfer" title="Transfer">
  {#snippet children()}
    <div class="application-settings-form flex w-full flex-col gap-6">
      {#if data?.is_localhost}
        <SettingsSection id="server-transfer-section" title="Transfer server" description="Move this server’s control-plane config to another coolify-rs instance (same physical host).">
          <Callout type="warning" title="Localhost cannot be transferred">
            The coolify-rs host (localhost) cannot be transferred between instances.
          </Callout>
        </SettingsSection>
      {:else if data}
        <SettingsSection id="server-transfer-section" title="Transfer server" description="Move this server’s control-plane config to another coolify-rs instance (same physical host).">
          {#snippet actions()}
            <span data-tooltip={exportTooltip}>
              {#if migration?.status === 'running'}
                <StatusBadge label="Transferring" type="warning" />
              {:else if data?.is_transferred}
                <StatusBadge label="Transferred away" type="warning" />
              {:else}
                <StatusBadge label="Managed here" type="success" />
              {/if}
            </span>
          {/snippet}

          <div class="flex flex-col gap-4">
            {#if !data.is_transferred && data.owner_marker_conflict}
              <Callout type="warning" title="Another instance may claim this server">
                The owner file on the server names a claim of
                {data.owner_marker_conflict.instance_url || 'an unknown instance'}, but it is older than the
                claim of this instance or its URL is not valid, so this instance keeps managing the server.
                If you did not transfer the server, check who can write files on it.
              </Callout>
            {/if}
            {#if data.is_transferred}
              <div>
                <h3 class="text-sm font-semibold text-neutral-950 dark:text-fg">Transfer back</h3>
                <p class="mt-1 text-xs leading-5 text-neutral-500 dark:text-fg-dim">
                  Undo the transfer. The instance that manages the server now transfers it back here,
                  with all changes made there. This archive is replaced only when the import here
                  succeeds. If the other instance is lost, use Enable management on the server page.
                </p>
              </div>
              <div class="flex flex-col gap-3 md:max-w-xl">
                <FormInput id="managerUrl" label="Instance that manages the server now" required placeholder="https://coolify.example.com" helper="The coolify-rs instance that this server was transferred to." bind:value={managerUrl} />
                <FormInput id="managerToken" type="password" label="API token of that instance" required placeholder="Paste token from that instance" autocomplete="off" helper="A token with root, or with read:sensitive and write, from an admin or owner of that instance." bind:value={managerToken} />
                <FormInput id="thisInstanceUrl" label="URL of this instance" required helper="The URL at which the other instance reaches this instance. This instance gives it a token that expires in 1 hour." bind:value={thisInstanceUrl} />
              </div>
              <div>
                <button type="button" class="button" disabled={busy} onclick={() => (confirming = 'back')}>
                  {busy ? 'Running checks…' : 'Transfer back'}
                </button>
              </div>
              {#if data.transfer_back?.requested_at}
                <Callout type="info" title="Transfer back started">
                  {data.transfer_back.from} started the transfer back at
                  {clock(data.transfer_back.requested_at, false)}. Reload this page in a minute.
                  When the transfer finishes, this instance manages the server again. If it does not, read
                  the transfer log of the server on the other instance.
                </Callout>
              {/if}
            {:else}
              <div>
                <h3 class="text-sm font-semibold text-neutral-950 dark:text-fg">Transfer to another instance</h3>
                <p class="mt-1 text-xs leading-5 text-neutral-500 dark:text-fg-dim">
                  Enter the target coolify-rs URL and an API token from that instance (root recommended).
                  This exports the server and imports it on the target, which then manages it. Management
                  is then disabled here.
                </p>
              </div>
              <div class="flex flex-col gap-3 md:max-w-xl">
                <FormInput id="targetUrl" label="Target instance URL" required placeholder="https://coolify.example.com" helper="Base URL of the other coolify-rs instance (no trailing path). It must use https." bind:value={targetUrl} />
                <FormInput id="targetToken" type="password" label="Target API token" required placeholder="Paste token from target instance" autocomplete="off" helper="Create a token on the target with root (or write + deploy). It is only used for this request." bind:value={targetToken} />
              </div>
              <div>
                <button type="button" class="button" disabled={busy || !data.is_owner} onclick={() => (confirming = 'migrate')}>
                  {busy ? 'Running checks…' : 'Transfer server'}
                </button>
              </div>
            {/if}
            {#if migration?.status === 'running'}
              <div>
                <Callout type="info" title="Transfer in progress">
                  The server is being transferred to {migration.target_url}. You can
                  leave this page; the transfer continues in the background.
                </Callout>
              </div>
            {:else if migration?.status === 'failed'}
              <Callout type="danger" title={migration.phase === 'checks' ? 'A check failed. The transfer did not start.' : 'Transfer failed'}>
                {#if log.length > 0}
                  See the transfer log below for details.
                {:else}
                  {migration.error}
                {/if}
              </Callout>
            {:else if migration?.status === 'unknown'}
              <Callout type="warning" title="Transfer result unknown">
                The transfer stopped before this instance could confirm the result. Check whether
                {migration.target_url} manages this server. If it does, call
                <code>POST /api/v1/servers/{uuid}/transfer/complete</code> on this instance.
                If it does not, transfer again.
              </Callout>
            {:else if migration?.status === 'succeeded'}
              <Callout type="success" title="Transfer finished">
                {migration.target_url} now manages this server. Automations are
                disabled on this instance.
              </Callout>
              {#if checklist.length > 0 || migration.target_server_url}
                <div data-testid="post-transfer-checklist">
                  <h3 class="text-sm font-semibold text-neutral-950 dark:text-fg">Finish the transfer on the target</h3>
                  <p class="mt-1 text-xs leading-5 text-neutral-500 dark:text-fg-dim">
                    The target shows these steps on the server page, checks them where it can, and keeps
                    their state.
                    {#if data.target_server_url}
                      <a href={data.target_server_url} target="_blank" rel="noopener noreferrer" class="underline">Open the server on the target</a>.
                    {/if}
                  </p>
                  {#if checklist.length > 0}
                    <ul class="mt-2 list-disc space-y-1 pl-5 text-xs leading-5 text-neutral-600 dark:text-fg-dim">
                      {#each checklist as step (step)}
                        <li>{step}</li>
                      {/each}
                    </ul>
                  {/if}
                </div>
              {/if}
            {/if}
            {#if log.length > 0}
              <div>
                <h3 class="text-sm font-semibold text-neutral-950 dark:text-fg">Transfer log</h3>
                <ol class="mt-2 flex flex-col gap-1 rounded-lg border border-neutral-200 bg-neutral-50 p-3 font-mono text-xs dark:border-coolgray-300 dark:bg-coolgray-100" data-testid="transfer-log">
                  {#each log as entry, i (`${i}-${entry.at}`)}
                    <li
                      class="flex gap-3"
                      class:text-success={entry.level === 'success'}
                      class:text-error={entry.level === 'error'}
                      class:text-warning={entry.level === 'warning'}
                      class:text-neutral-600={entry.level === 'info'}
                      class:dark:text-fg-dim={entry.level === 'info'}
                    >
                      <span class="shrink-0 text-neutral-400">{clock(entry.at, true)}</span>
                      <span class="break-words">{entry.message}</span>
                    </li>
                  {/each}
                </ol>
              </div>
            {/if}
            {#if lastWarnings.length > 0}
              <Callout type="warning" title="Warnings">
                <ul class="mt-2 list-disc space-y-1 pl-5 text-sm">
                  {#each lastWarnings as warning (warning)}
                    <li>{warning}</li>
                  {/each}
                </ul>
              </Callout>
            {/if}
          </div>
        </SettingsSection>
      {/if}
    </div>
  {/snippet}
</ServerFrame>

{#if confirming === 'migrate'}
  <ConfirmModal
    title="Transfer this server?"
    confirmWithText={false}
    confirmWithPassword={false}
    step2ButtonText="Transfer server"
    warningMessage="All secrets of this server and its resources are sent to the target instance: private keys, credentials, and every environment variable value, also locked ones. Continue only if you trust that instance."
    actions={[
      'coolify-rs first checks this server, the target instance, and the token. If a check fails, nothing is changed.',
      'The server will be transferred to the target instance.',
      'Automations for this server will be disabled on this instance.',
    ]}
    onconfirm={migrateServer}
    onclose={() => (confirming = '')}
  />
{:else if confirming === 'back'}
  <ConfirmModal
    title="Transfer this server back?"
    confirmWithText={false}
    confirmWithPassword={false}
    step2ButtonText="Transfer back"
    warningMessage={`This instance sends a new API token of your team to <strong data-testid="transfer-back-host">${managerHost.replace(/[<>&"]/g, '')}</strong>. Continue only if you trust that instance.`}
    actions={[
      'The other instance checks the server, this instance, and the tokens. If a check fails, nothing changes.',
      'The other instance transfers the server, its resources, and its credentials back here.',
      'This archive is replaced by the transferred server, and the other instance keeps an archive.',
    ]}
    onconfirm={transferBack}
    onclose={() => (confirming = '')}
  />
{/if}
