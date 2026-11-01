<script lang="ts">
  // Port of upstream livewire/server/docker-cleanup.blade.php and server/docker-cleanup-executions.blade.php.
  import Reicon from '../components/Reicon.svelte';
  import Callout from '../components/Callout.svelte';
  import ConfirmModal from '../components/ConfirmModal.svelte';
  import Empty from '../components/Empty.svelte';
  import Helper from '../components/Helper.svelte';
  import FormInput from '../components/FormInput.svelte';
  import Listbox from '../components/Listbox.svelte';
  import ServerFrame from '../components/ServerFrame.svelte';
  import SettingsSection from '../components/SettingsSection.svelte';
  import StatusBadge from '../components/StatusBadge.svelte';
  import UnsavedBar from '../components/UnsavedBar.svelte';
  import { ApiError } from '../lib/api';
  import { get, post, failure } from '../lib/serverApi';
  import { session } from '../lib/session.svelte';
  import { notify } from '../lib/toast.svelte';

  let { uuid }: { uuid: string } = $props();
  type Execution = { uuid: string; status: string; message: string | null; created_at: string; finished_at: string | null };
  type Data = {
    docker_cleanup_frequency: string;
    docker_cleanup_threshold: number;
    force_docker_cleanup: boolean;
    delete_unused_volumes: boolean;
    delete_unused_networks: boolean;
    disable_application_image_retention: boolean;
    server_timezone: string | null;
    is_cleanup_stale: boolean;
    is_scheduler_healthy: boolean;
    executions: Execution[];
  };
  let data = $state<Data | null>(null);
  let frequency = $state('');
  let threshold = $state('');
  let saved = $state({ frequency: '', threshold: '' });
  let saving = $state(false);
  let error = $state<ApiError | null>(null);
  let confirming = $state(false);
  let selected = $state<string | null>(null);
  let timezone = $state('UTC');
  let page = $state(1);
  const base = $derived(`/server-page/${encodeURIComponent(uuid)}/docker-cleanup`);
  const canEdit = $derived(session.isAdmin);
  const dirty = $derived(frequency !== saved.frequency || threshold !== saved.threshold);
  let poll: ReturnType<typeof setInterval> | undefined;

  async function refresh() {
    const d = await get<Data>(`${base}?poll=1`);
    if (data) data.executions = d.executions;
    return d;
  }
  async function load() {
    const d = await get<Data>(base);
    data = d;
    timezone = d.server_timezone ?? 'UTC';
    frequency = saved.frequency = d.docker_cleanup_frequency;
    threshold = saved.threshold = String(d.docker_cleanup_threshold);
    clearInterval(poll);
    poll = setInterval(refresh, 5000);
  }
  $effect(() => () => clearInterval(poll));

  const payload = () => ({
    docker_cleanup_frequency: frequency,
    docker_cleanup_threshold: threshold,
    force_docker_cleanup: data?.force_docker_cleanup,
    delete_unused_volumes: data?.delete_unused_volumes,
    delete_unused_networks: data?.delete_unused_networks,
    disable_application_image_retention: data?.disable_application_image_retention,
  });
  async function save(instant = false) {
    saving = true;
    error = null;
    try {
      await post(base, instant ? { ...payload(), instant: true } : payload());
      saved = { frequency, threshold };
      notify.success('Server updated.');
    } catch (e) {
      error = failure(e);
    } finally {
      saving = false;
    }
  }
  async function run() {
    try {
      const r = await post<{ message: string }>(`${base}/run`);
      notify.success(r.message);
    } catch (e) {
      failure(e);
    }
  }
  function download(x: Execution) {
    const a = document.createElement('a');
    a.href = URL.createObjectURL(new Blob([x.message ?? ''], { type: 'text/plain' }));
    a.download = `docker-cleanup-${x.uuid}.log`;
    a.click();
    URL.revokeObjectURL(a.href);
  }
  const execStatus = (s: string) => (({ success: ['Success', 'success'], running: ['In progress', 'warning'], failed: ['Failed', 'error'] } as Record<string, [string, 'success' | 'warning' | 'error']>)[s] ?? [s.replace(/\b\w/g, (c) => c.toUpperCase()), 'neutral']) as [string, 'success' | 'warning' | 'error' | 'neutral'];
  /** Stored timestamps are UTC. */
  const utc = (t: string) => new Date(/Z$|[+-]\d\d:?\d\d$/.test(t) ? t : `${t.replace(' ', 'T')}Z`);
  /** `formatDateInServerTimezone`. */
  function serverTime(iso: string): string {
    const parts = new Intl.DateTimeFormat('en-CA', { timeZone: timezone, year: 'numeric', month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit', second: '2-digit', hour12: false, timeZoneName: 'short' }).formatToParts(utc(iso));
    const p = (t: string) => parts.find((x) => x.type === t)?.value ?? '';
    return `${p('year')}-${p('month')}-${p('day')} ${p('hour')}:${p('minute')}:${p('second')} ${p('timeZoneName')}`;
  }
  /** `calculateDuration`. */
  function duration(a: string, b: string | null): string {
    if (!b) return '';
    const t = Math.floor((utc(b).getTime() - utc(a).getTime()) / 1000);
    const pad = (n: number) => String(n).padStart(2, '0');
    const [d, h, m, s] = [Math.floor(t / 86400), Math.floor((t % 86400) / 3600), Math.floor((t % 3600) / 60), t % 60];
    if (d > 0) return `${d}d ${pad(h)}h ${pad(m)}m ${pad(s)}s`;
    if (h > 0) return `${pad(h)}h ${pad(m)}m ${pad(s)}s`;
    return `${pad(m)}m ${pad(s)}s`;
  }
  /** Carbon `diffForHumans`. */
  function ago(iso: string | null): string {
    if (!iso) return '';
    const t = Math.floor((Date.now() - utc(iso).getTime()) / 1000);
    const units: [number, string][] = [[31536000, 'year'], [2592000, 'month'], [604800, 'week'], [86400, 'day'], [3600, 'hour'], [60, 'minute'], [1, 'second']];
    for (const [n, u] of units) {
      if (t >= n) {
        const c = Math.floor(t / n);
        return `${c} ${u}${c === 1 ? '' : 's'} ago`;
      }
    }
    return '0 seconds ago';
  }
  const lines = (x: Execution) => (x.message ? x.message.split('\n') : ['Waiting for execution output...']);
</script>

<ServerFrame {uuid} active="docker-cleanup" title="Docker Cleanup" onloaded={load}>
  {#snippet children()}
    {#if data}
      <div class="application-settings-form flex w-full flex-col gap-6">
        <form
          class="contents"
          onsubmit={(e) => {
            e.preventDefault();
            save();
          }}
        >
          <UnsavedBar {dirty} {saving} onsave={() => save()} onreset={() => ((frequency = saved.frequency), (threshold = saved.threshold), (error = null))} />
          <SettingsSection id="docker-cleanup-overview-section" title="Docker cleanup" description="Remove unused Docker data and keep disk usage under control.">
            {#snippet actions()}
              {#if canEdit}
                <button type="button" class="button button-highlighted" onclick={() => (confirming = true)}>Run cleanup</button>
              {/if}
            {/snippet}
            {#if data.is_cleanup_stale}
              <Callout type="warning" title="Docker cleanup may be stalled">
                The last cleanup ran {data.executions[0] ? ago(data.executions[0].created_at) : 'at an unknown time'}.
                {#if !data.is_scheduler_healthy}The scheduled job manager also appears inactive.{/if}
                Run
                <code class="rounded bg-black/10 px-1 dark:bg-white/10">php artisan cleanup:redis --clear-locks</code>
                on the coolify-rs instance to clear stale locks.
              </Callout>
            {:else}
            <div class="flex items-start gap-3">
                <div class="flex size-9 shrink-0 items-center justify-center rounded-lg bg-neutral-100 text-neutral-500 dark:bg-white/[0.06] dark:text-fg-dim">
                  <Reicon name="storages" class="size-4" />
                </div>
                <div>
                  <p class="text-sm font-medium text-neutral-950 dark:text-fg">Scheduled maintenance</p>
                  <p class="mt-1 text-xs leading-5 text-neutral-500 dark:text-fg-dim">Cleanup runs automatically using the schedule and threshold configured below.</p>
                </div>
              </div>
            {/if}
            <section id="docker-cleanup-configuration-section" class="mt-5 border-t border-neutral-200 pt-5 dark:border-white/[0.07]">
              <div class="mb-3 flex flex-wrap items-center justify-between gap-3">
                <h3 class="flex items-center gap-1.5 text-sm! font-semibold! text-black dark:text-fg">
                  Schedule
                  <Helper helper="Choose when cleanup runs and whether it should wait for a disk threshold." />
                </h3>
              </div>
              <div class="grid gap-4 lg:grid-cols-3">
                <FormInput disabled={!canEdit} placeholder="0 0 * * *" id="dockerCleanupFrequency" label="Cleanup frequency" required helper="Cron expression or preset such as hourly, daily, weekly, monthly, or yearly." bind:value={frequency} error={error?.field('docker_cleanup_frequency')} />
                <Listbox
                  id="forceDockerCleanup"
                  label="Cleanup trigger"
                  helper="Forced cleanup runs on every schedule without waiting for the threshold."
                  disabled={!canEdit}
                  bind:value={data.force_docker_cleanup}
                  onchange={() => save(true)}
                  options={[
                    { value: false, label: 'Only above disk threshold' },
                    { value: true, label: 'Run on every schedule' },
                  ]}
                />
                {#if !data.force_docker_cleanup}
                  <FormInput disabled={!canEdit} id="dockerCleanupThreshold" type="number" min="1" max="99" label="Disk threshold" required helper="Run cleanup after disk usage exceeds this percentage." bind:value={threshold} error={error?.field('docker_cleanup_threshold')} />
                {/if}
              </div>
            </section>
            <section id="docker-cleanup-advanced-section" class="mt-5 border-t border-neutral-200 pt-5 dark:border-white/[0.07]">
              <div class="mb-3 flex flex-wrap items-center justify-between gap-3">
                <h3 class="flex items-center gap-1.5 text-sm! font-semibold! text-black dark:text-fg">
                  Advanced cleanup
                  <Helper helper="Control destructive cleanup behavior and application image retention." />
                </h3>
              </div>
              <Callout type="warning" title="These options can remove recoverable data">
                Unused volumes may contain data from stopped containers, while removing retained images disables rollback to older application versions.
              </Callout>
              <div class="mt-4 grid gap-4 lg:grid-cols-3">
                <Listbox id="deleteUnusedVolumes" label="Unused volumes" helper="Permanently remove volumes not attached to running containers." disabled={!canEdit} bind:value={data.delete_unused_volumes} onchange={() => save(true)} options={[{ value: false, label: 'Keep unused volumes' }, { value: true, label: 'Delete unused volumes' }]} />
                <Listbox id="deleteUnusedNetworks" label="Unused networks" helper="Remove Docker networks not attached to running containers." disabled={!canEdit} bind:value={data.delete_unused_networks} onchange={() => save(true)} options={[{ value: false, label: 'Keep unused networks' }, { value: true, label: 'Delete unused networks' }]} />
                <Listbox id="disableApplicationImageRetention" label="Application images" helper="Choose Keep retained images to follow each application’s Images to keep setting under Rollback. Delete all old images ignores that setting." disabled={!canEdit} bind:value={data.disable_application_image_retention} onchange={() => save(true)} options={[{ value: false, label: 'Keep retained images' }, { value: true, label: 'Delete all old images' }]} />
              </div>
            </section>
          </SettingsSection>
        </form>

        <SettingsSection id="docker-cleanup-executions-section" title="Recent executions" description="Review cleanup status, duration, and command output." flush>
          {#each data.executions as x (x.uuid)}
            <div class="border-b border-neutral-200 last:border-b-0 dark:border-white/[0.08]">
              <button
                type="button"
                onclick={() => ((selected = selected === x.uuid ? null : x.uuid), (page = 1))}
                class="flex w-full items-center gap-4 px-4 py-3 text-left transition-colors hover:bg-neutral-50 dark:hover:bg-white/[0.03]"
              >
                <StatusBadge label={execStatus(x.status)[0]} type={execStatus(x.status)[1]} />
                <div class="min-w-0 flex-1">
                  <p class="text-sm font-medium text-neutral-950 dark:text-fg">{serverTime(x.created_at)}</p>
                  <p class="mt-0.5 text-xs text-neutral-500 dark:text-fg-dim">
                    {#if x.status === 'running'}Cleanup is currently running{:else}{duration(x.created_at, x.finished_at)} · finished {ago(x.finished_at)}{/if}
                  </p>
                </div>
                <Reicon name="chevron-down" class="size-4 shrink-0 text-neutral-400 transition-transform {selected === x.uuid ? 'rotate-180' : ''}" />
              </button>
              {#if selected === x.uuid}
                <div class="border-t border-neutral-200 bg-neutral-50/60 p-4 dark:border-white/[0.08] dark:bg-black/20">
                  <div class="mb-3 flex items-center justify-between gap-3">
                    <p class="text-xs font-medium uppercase tracking-wide text-neutral-500 dark:text-fg-dim">Execution output</p>
                    {#if x.message}
                      <button type="button" class="button" onclick={() => download(x)}>Download logs</button>
                    {/if}
                  </div>
                  <pre class="max-h-80 overflow-auto rounded-lg bg-white text-neutral-700 p-4 font-mono text-xs leading-5 ring-1 ring-neutral-200 dark:bg-black/40 dark:text-fg-dim dark:ring-white/[0.08]">{lines(x).slice(0, page * 100).join('\n')}</pre>
                  {#if lines(x).length > page * 100}
                    <div class="mt-3"><button type="button" class="button" onclick={() => page++}>Load more</button></div>
                  {/if}
                </div>
              {/if}
            </div>
          {:else}
            <Empty size="sm" title="No cleanup executions" description="Run a manual cleanup or wait for the next scheduled execution." iconName="storages" />
          {/each}
        </SettingsSection>
      </div>
    {/if}
  {/snippet}
</ServerFrame>

{#if confirming}
  <ConfirmModal
    title="Confirm Docker Cleanup?"
    actions={[
      'Deletes stopped containers managed by coolify-rs.',
      'Deletes unused images and clears the build cache.',
      'Removes old coolify-rs helper images.',
      'May delete unused volumes or networks when those options are enabled.',
    ]}
    confirmWithText={false}
    step2ButtonText="Run Docker Cleanup"
    onconfirm={run}
    onclose={() => (confirming = false)}
  />
{/if}
