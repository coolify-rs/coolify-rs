<script lang="ts">
  // Port of upstream livewire/image-scan/server-image-scanning.blade.php
  // (ImageScan\ServerImageScanning): image scanning of a server (on or off, the scanner, the
  // scanner limits, the scan rules of the server), then its host package vulnerabilities.
  // Polls while Coolify gets the scanner (`ImageScanUpdated` of the prepare job).
  import { untrack } from 'svelte';
  import CheckpointItem from '../components/CheckpointItem.svelte';
  import ConfirmModal from '../components/ConfirmModal.svelte';
  import Empty from '../components/Empty.svelte';
  import Listbox from '../components/Listbox.svelte';
  import ServerFrame from '../components/ServerFrame.svelte';
  import SettingsSection from '../components/SettingsSection.svelte';
  import SettingsSubsection from '../components/SettingsSubsection.svelte';
  import HostVulnerabilities from '../components/imageScan/HostVulnerabilities.svelte';
  import ScanSettings from '../components/imageScan/ScanSettings.svelte';
  import { request } from '../lib/api';
  import { run } from '../lib/imageScanApi';
  import { failure } from '../lib/serverApi';

  interface Option {
    value: string;
    label: string;
  }
  interface Data {
    name: string;
    can_update: boolean;
    enabled: boolean;
    memory_limit: string;
    memory_options: Option[];
    timeout_options: Option[];
    cpu_options: Option[];
    values: { scanner_memory_mb: string; scanner_syft_timeout: string; scanner_grype_timeout: string; scanner_cpus: string };
    prepare_state: string | null;
    failed_step: string | null;
    registries: string;
    disk_used: string | null;
    has_scanner_data: boolean;
    tool_versions: string;
    built_at: string | null;
    cpu_limit: string;
  }

  let { uuid }: { uuid: string; sub?: string } = $props();

  const url = $derived(`/ui/server-page/${encodeURIComponent(uuid)}/image-scanning`);
  let data = $state<Data | null>(null);
  let modal = $state<'delete' | 'toggle' | null>(null);

  async function load() {
    try {
      data = await request<Data>('GET', url);
    } catch (e) {
      failure(e);
    }
  }

  $effect(() => {
    void uuid;
    untrack(() => void load());
  });
  // Coolify gets the scanner in a job: poll until it is ready or failed.
  $effect(() => {
    const state = data?.prepare_state;
    if (!data?.enabled || (state !== 'pulling' && state !== 'updating_database')) return;
    const timer = setInterval(() => void load(), 5000);
    return () => clearInterval(timer);
  });

  async function act(path: string) {
    await run(() => request<{ message: string }>('POST', `${url}/${path}`, {}));
    await load();
  }
  async function saveLimit(field: keyof Data['values'], value: string) {
    if (data) data.values[field] = value;
    await run(() => request<{ message: string }>('PATCH', `${url}/limits`, { field, value }));
    await load();
  }

  const costText = $derived(
    data ? `Scans use up to ${data.cpu_limit} CPU and ${data.memory_limit} of memory while they run, and about 1.2 GB of disk, plus temporary space for one image.` : '',
  );
  const pullState = $derived.by(() => {
    switch (data?.prepare_state) {
      case 'pulling':
        return 'running';
      case 'updating_database':
      case 'ready':
        return 'success';
      case 'failed':
        return ['pulling', 'setup', 'connection'].includes(data?.failed_step ?? '') ? 'error' : 'success';
      default:
        return 'idle';
    }
  });
  const databaseState = $derived.by(() => {
    switch (data?.prepare_state) {
      case 'pulling':
        return 'pending';
      case 'updating_database':
        return 'running';
      case 'ready':
        return 'success';
      case 'failed':
        return data?.failed_step === 'updating_database' ? 'error' : 'pending';
      default:
        return 'idle';
    }
  });
  const pullDescription = $derived(
    pullState === 'running'
      ? 'Pulling the scanner images.'
      : pullState === 'success'
        ? 'The scanner images are on this server.'
        : pullState === 'error'
          ? data?.failed_step === 'setup'
            ? 'Coolify could not set up the scanner storage on the server. Check that the SSH user can run Docker commands.'
            : data?.failed_step === 'connection'
              ? 'Coolify could not connect to the server. Check that the server is reachable, then try again.'
              : `Coolify could not pull the scanner images. Check that the server can reach ${data?.registries}.`
          : 'Not done yet.',
  );
  const databaseDescription = $derived(
    databaseState === 'running'
      ? 'Downloading the vulnerability database.'
      : databaseState === 'success'
        ? data?.built_at
          ? `Database built ${data.built_at}.`
          : 'The vulnerability database is up to date.'
        : databaseState === 'error'
          ? 'Coolify could not download the vulnerability database.'
          : 'Not done yet.',
  );
  const sectionHelper = $derived(
    data
      ? [
          'Scan the images on this server for known vulnerabilities before and after deployments. Coolify also scans the operating system packages of the server every day.',
          `Scanner tools: ${data.tool_versions}.`,
          'Coolify downloads the latest vulnerability database every day, between 02:00 and 05:00 server time.' +
            (data.built_at ? ` Database built ${data.built_at}.` : ''),
          data.disk_used ? `Disk used by scanner data: ${data.disk_used}.` : null,
          costText,
        ]
          .filter((l): l is string => !!l)
          // `e($line)` joined with `<br><br>`.
          .map((l) => l.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;'))
          .join('<br><br>')
      : '',
  );
</script>

<ServerFrame {uuid} active="security" sub="vulnerabilities" title="Vulnerabilities">
  {#snippet children()}
    <div class="application-settings-form flex w-full min-w-0 flex-col gap-6">
      {#if data}
        <SettingsSection id="server-image-scanning-section" title="Image scanning" description={sectionHelper}>
          {#snippet actions()}
            {#if data?.can_update}
              {#if data.enabled && (data.prepare_state === null || data.prepare_state === 'failed')}
                <button type="button" class="button" onclick={() => act('prepare')}>
                  {data.prepare_state === 'failed' ? 'Try again' : 'Get scanner'}
                </button>
              {/if}
              <!-- Disabling forgets the cached state, but the files stay on the server. -->
              {#if !data.enabled || data.has_scanner_data}
                <button type="button" class="button button-error" onclick={() => (modal = 'delete')}>Delete scanner data</button>
              {/if}
              <div>
                <button type="button" class="button {data.enabled ? '' : 'button-highlighted'}" onclick={() => (modal = 'toggle')}>
                  {data.enabled ? 'Disable' : 'Enable'}
                </button>
              </div>
            {/if}
          {/snippet}

          {#if !data.enabled}
            {#if data.can_update}
              <Empty size="sm" iconName="shield-alert" title="Image scanning is off for {data.name}." description={costText} />
            {:else}
              <Empty size="sm" iconName="shield-alert" title="Image scanning is off for {data.name}." description={costText}>
                {#snippet contents()}
                  <span class="text-[13px] text-neutral-500 dark:text-fg-faint">Ask a team admin to turn it on.</span>
                {/snippet}
              </Empty>
            {/if}
          {:else if data.prepare_state !== 'ready'}
            <div class="divide-y divide-neutral-200 rounded-lg border border-neutral-200 dark:divide-white/[0.08] dark:border-white/[0.08]">
              <CheckpointItem title="Get scanner" icon="download" status={pullState} description={pullDescription} />
              <CheckpointItem title="Update database" icon="database" status={databaseState} description={databaseDescription} />
            </div>
          {/if}
          <div class={!data.enabled || data.prepare_state !== 'ready' ? 'mt-5 border-t border-neutral-200 pt-5 dark:border-white/[0.07]' : ''}>
            <SettingsSubsection
              id="image-scan-limits-section"
              title="Scanner limits"
              helper="Limits of each scanner step on this server. Raise one when a scan stops because it took too long or ran out of memory. A new limit applies to the next scan."
              first
            >
              <div class="grid gap-4 md:grid-cols-2 xl:grid-cols-4">
                <Listbox
                  id="scannerSyftTimeout"
                  label="Syft time limit"
                  disabled={!data.can_update}
                  helper="Time to list the packages of one image. Large images need more time."
                  options={data.timeout_options}
                  value={data.values.scanner_syft_timeout}
                  onchange={(v) => saveLimit('scanner_syft_timeout', String(v))}
                />
                <Listbox
                  id="scannerGrypeTimeout"
                  label="Grype time limit"
                  disabled={!data.can_update}
                  helper="Time to match the packages of one image with the vulnerability database."
                  options={data.timeout_options}
                  value={data.values.scanner_grype_timeout}
                  onchange={(v) => saveLimit('scanner_grype_timeout', String(v))}
                />
                <Listbox
                  id="scannerCpus"
                  label="Scanner CPU limit"
                  disabled={!data.can_update}
                  helper="CPUs that each scanner step can use. Values above the CPU count of this server are not listed."
                  options={data.cpu_options}
                  value={data.values.scanner_cpus}
                  onchange={(v) => saveLimit('scanner_cpus', String(v))}
                />
                <Listbox
                  id="scannerMemoryMb"
                  label="Scanner memory limit"
                  disabled={!data.can_update}
                  helper="Memory that each scanner step can use. Values above the memory of this server are not listed. The server must also have this much free memory when a scan runs."
                  options={data.memory_options}
                  value={data.values.scanner_memory_mb}
                  onchange={(v) => saveLimit('scanner_memory_mb', String(v))}
                />
              </div>
            </SettingsSubsection>
            <div class="mt-5 border-t border-neutral-200 pt-5 dark:border-white/[0.07]">
              {#key uuid}
                <ScanSettings scope="server" {uuid} embedded />
              {/key}
            </div>
          </div>
        </SettingsSection>
      {/if}

      {#key uuid}
        <HostVulnerabilities {uuid} />
      {/key}
    </div>
  {/snippet}
</ServerFrame>

{#if modal === 'delete'}
  <ConfirmModal
    title="Delete scanner data?"
    actions={[
      'Coolify removes the stored SBOMs, the vulnerability database, and the scanner images from this server.',
      'Scan results in Coolify stay. The next scan gets the scanner again.',
    ]}
    confirmWithText={false}
    confirmWithPassword={false}
    step2ButtonText="Delete scanner data"
    onconfirm={() => act('delete-data')}
    onclose={() => (modal = null)}
  />
{:else if modal === 'toggle' && data}
  <ConfirmModal
    title={data.enabled ? 'Disable image scanning?' : 'Enable image scanning?'}
    warningMessage={data.enabled
      ? 'New deployments and running images on this server are no longer checked for vulnerabilities. You can enable it again at any time.'
      : 'Scans run on this server and use its resources. You can disable it again at any time.'}
    actions={data.enabled
      ? ['Coolify stops scanning the images and the operating system packages of this server.', 'Scan results and scanner data on the server stay.']
      : [costText]}
    confirmWithText={false}
    confirmWithPassword={false}
    step2ButtonText={data.enabled ? 'Disable image scanning' : 'Enable image scanning'}
    onconfirm={() => act(data?.enabled ? 'disable' : 'enable')}
    onclose={() => (modal = null)}
  />
{/if}
