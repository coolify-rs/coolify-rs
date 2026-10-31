<script lang="ts">
  // Port of upstream livewire/server/github-runners.blade.php and github-runner-executions.blade.php.
  import { onDestroy } from 'svelte';
  import BetaBadge from '../components/BetaBadge.svelte';
  import Callout from '../components/Callout.svelte';
  import ConfirmModal from '../components/ConfirmModal.svelte';
  import Empty from '../components/Empty.svelte';
  import FormInput from '../components/FormInput.svelte';
  import Listbox from '../components/Listbox.svelte';
  import ServerActivityDialog from '../components/ServerActivityDialog.svelte';
  import ServerFrame from '../components/ServerFrame.svelte';
  import SettingsSection from '../components/SettingsSection.svelte';
  import StatusBadge from '../components/StatusBadge.svelte';
  import UnsavedBar from '../components/UnsavedBar.svelte';
  import { ApiError } from '../lib/api';
  import { diffForHumans } from '../lib/format';
  import { link, router } from '../lib/router.svelte';
  import { get, post, failure, kebab } from '../lib/serverApi';
  import { session } from '../lib/session.svelte';
  import { notify } from '../lib/toast.svelte';

  let { uuid }: { uuid: string } = $props();

  type App = { id: number; uuid: string; name: string; organization: string; html_url: string; missing: string[] };
  type Config = {
    github_app_id: number;
    labels: string[];
    is_enabled: boolean;
    max_runners: number;
    docker_mode: string;
    runner_image: string | null;
    cpu_limit: string | null;
    memory_limit: string | null;
    capacity_wait_timeout: number;
    idle_timeout: number;
    job_timeout: number;
    is_dedicated: boolean;
    allow_pull_requests: boolean;
  };
  type Data = { is_localhost: boolean; can_build: boolean; is_build_server: boolean; apps: App[]; config: Config | null };
  type Execution = {
    id: number;
    uuid: string;
    status: string;
    repository_full_name: string | null;
    workflow_name: string | null;
    job_name: string | null;
    conclusion: string | null;
    error_message: string | null;
    workflow_job_html_url: string | null;
    created_at: string;
    started_at: string | null;
    completed_at: string | null;
  };
  type Form = {
    app: string;
    labels: string;
    maxRunners: string;
    dockerMode: string;
    runnerImage: string;
    cpuLimit: string;
    memoryLimit: string;
    capacityWaitTimeout: string;
    idleTimeout: string;
    jobTimeout: string;
    isDedicated: string;
    allowPullRequests: string;
  };

  const RUNNER_IMAGE = 'ghcr.io/actions/actions-runner:latest';
  const SYSBOX_VERSION = '0.7.1';
  let data = $state<Data | null>(null);
  let form = $state<Form>(defaults(null, []));
  let saved = $state<Form>(defaults(null, []));
  let saving = $state(false);
  let error = $state<ApiError | null>(null);
  let confirmEnable = $state(false);
  let cancelling = $state<number | null>(null);
  let sysbox = $state<boolean | null>(null);
  let dialog = $state<number | null>(null);
  let executions = $state<Execution[]>([]);
  let poll: ReturnType<typeof setInterval> | undefined;

  const url = $derived(`/server-page/${encodeURIComponent(uuid)}/github-runners`);
  const canEdit = $derived(session.isAdmin);
  const dirty = $derived(JSON.stringify(form) !== JSON.stringify(saved));
  const enabled = $derived(!!data?.config?.is_enabled);
  const selectedApp = $derived(data?.apps.find((a) => String(a.id) === form.app) ?? null);

  function defaults(config: Config | null, apps: App[]): Form {
    if (!config) {
      const app = apps.find((a) => a.missing.length === 0) ?? apps[0];
      return { app: app ? String(app.id) : '', labels: 'coolify', maxRunners: '2', dockerMode: 'none', runnerImage: '', cpuLimit: '', memoryLimit: '', capacityWaitTimeout: '60', idleTimeout: '10', jobTimeout: '360', isDedicated: 'false', allowPullRequests: 'false' };
    }
    return {
      app: String(config.github_app_id),
      labels: (config.labels ?? []).join(', '),
      maxRunners: String(config.max_runners),
      dockerMode: config.docker_mode,
      runnerImage: config.runner_image ?? '',
      cpuLimit: config.cpu_limit ?? '',
      memoryLimit: config.memory_limit ?? '',
      capacityWaitTimeout: String(config.capacity_wait_timeout),
      idleTimeout: String(config.idle_timeout),
      jobTimeout: String(config.job_timeout),
      isDedicated: String(config.is_dedicated),
      allowPullRequests: String(config.allow_pull_requests),
    };
  }
  const payload = () => ({
    github_app_id: form.app,
    labels: form.labels,
    max_runners: form.maxRunners,
    docker_mode: form.dockerMode,
    runner_image: form.runnerImage || null,
    cpu_limit: form.cpuLimit || null,
    memory_limit: form.memoryLimit || null,
    capacity_wait_timeout: form.capacityWaitTimeout,
    idle_timeout: form.idleTimeout,
    job_timeout: form.jobTimeout,
    is_dedicated: form.isDedicated === 'true',
    allow_pull_requests: form.allowPullRequests === 'true',
  });

  async function loadExecutions() {
    const r = await get<{ executions: Execution[] }>(`${url}/executions`).catch(() => null);
    if (r) executions = r.executions;
  }
  async function load() {
    const d = await get<Data>(url);
    if (d.is_localhost) {
      router.go(`/server/${uuid}`);
      return;
    }
    data = d;
    form = defaults(d.config, d.apps);
    saved = { ...form };
    clearInterval(poll);
    if (d.config?.is_enabled) {
      await loadExecutions();
      poll = setInterval(loadExecutions, 10000);
    }
    checkSysbox();
  }
  /** `x-effect`: checked when the mode is first Sysbox. */
  async function checkSysbox() {
    if (form.dockerMode !== 'sysbox' || sysbox !== null) return;
    const r = await get<{ installed: boolean }>(`${url}/sysbox`).catch(() => null);
    if (r) sysbox = r.installed;
    // The request syncs the form: `wire:dirty` compares with the last synced values.
    saved = { ...form };
  }
  onDestroy(() => clearInterval(poll));

  function toasts(r: { message?: string; warning?: string | null }) {
    // Livewire delivers `success` before the `warning` dispatched earlier in the action.
    if (r.message) notify.success(r.message);
    if (r.warning) notify.warning(r.warning);
  }
  async function save() {
    saving = true;
    error = null;
    try {
      toasts(await post<{ message: string; warning?: string }>(url, payload()));
      await load();
    } catch (e) {
      error = failure(e);
      // A failed request also syncs the form, so the bar closes.
      saved = { ...form };
      if (error?.field('docker_mode')) sysbox = false;
    } finally {
      saving = false;
    }
  }
  async function toggle() {
    error = null;
    try {
      toasts(await post<{ message: string; warning?: string }>(`${url}/toggle`, payload()));
      await load();
    } catch (e) {
      error = failure(e);
      // A validation message of the first enable is shown as a toast: the form is not on the page yet.
      if (error && Object.keys(error.errors).length) notify.failure(error.message);
    }
  }
  async function installSysbox() {
    try {
      dialog = (await post<{ activity_id: number }>(`${url}/sysbox/install`)).activity_id;
    } catch (e) {
      failure(e);
    }
  }
  async function cancel(id: number) {
    try {
      toasts(await post<{ message?: string }>(`${url}/executions/${id}/cancel`));
      await loadExecutions();
    } catch (e) {
      failure(e);
    }
  }

  // GithubRunnerStatus::label
  const statusLabel = (s: string) => (s === 'idle' ? 'Waiting for job' : s === 'timed_out' ? 'Timed out' : s.charAt(0).toUpperCase() + s.slice(1));
  function statusType(e: Execution): 'success' | 'neutral' | 'error' | 'warning' {
    if (e.status === 'completed') return e.conclusion === 'success' ? 'success' : 'neutral';
    if (e.status === 'failed' || e.status === 'timed_out') return 'error';
    if (e.status === 'cancelled') return 'neutral';
    return 'warning';
  }
  const isActive = (e: Execution) => ['queued', 'provisioning', 'idle', 'running'].includes(e.status);
  /** `GithubRunnerExecution::duration`: `started_at->diffForHumans(completed_at ?? now, true)`. */
  function ran(e: Execution): string {
    if (!e.started_at) return '';
    const t = (v: string) => new Date(v.endsWith('Z') || v.includes('+') ? v : `${v}Z`).getTime();
    const seconds = Math.abs(Math.floor(((e.completed_at ? t(e.completed_at) : Date.now()) - t(e.started_at)) / 1000));
    const units: [string, number][] = [['year', 31536000], ['month', 2592000], ['week', 604800], ['day', 86400], ['hour', 3600], ['minute', 60], ['second', 1]];
    for (const [name, size] of units) {
      const n = Math.floor(seconds / size);
      if (n >= 1) return `${n} ${name}${n === 1 ? '' : 's'}`;
    }
    return '1 second';
  }
  /** `getPermissionsPath`. */
  const permissionsPath = (a: App) => `${a.html_url.replace(/\/+$/, '')}/organizations/${encodeURIComponent(a.organization)}/settings/apps/${encodeURIComponent(kebab(a.name))}/permissions`;
</script>

{#snippet externalLink()}
  <svg class="inline-flex w-3 h-3 dark:text-neutral-400 text-black" fill="none" stroke="currentColor" viewBox="0 0 24 24">
    <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M10 6H6a2 2 0 00-2 2v10a2 2 0 002 2h10a2 2 0 002-2v-4M14 4h6m0 0v6m0-6L10 14" />
  </svg>
{/snippet}
{#snippet generalActions()}
  <BetaBadge />
  <a class="button" href="/server/{uuid}" use:link>
    General settings
    {@render externalLink()}
  </a>
{/snippet}
{#snippet beta()}
  <BetaBadge />
{/snippet}
{#snippet enabledActions()}
  <BetaBadge />
  {#if canEdit}
    <button type="button" class="button" onclick={toggle}>Disable runners</button>
  {/if}
{/snippet}

<ServerFrame {uuid} active="github-runners" title="GitHub Runners" onloaded={load}>
  {#snippet children()}
    {#if dialog !== null}
      <ServerActivityDialog {uuid} activityId={dialog} title="Sysbox Installation Logs" onclose={() => ((dialog = null), (sysbox = null), checkSysbox())} />
    {/if}
    {#if data}
      <div class="application-settings-form flex w-full flex-col gap-6">
        {#if !data.can_build}
          <SettingsSection id="github-runners-section" title="GitHub Actions runners" description="Each workflow job runs in a new container that is deleted after the job." actions={generalActions}>
            <Empty size="sm" title="This server is not enabled for builds" description="GitHub Actions runners need the Builds only or the Deployments and builds role. Change the server role in the General settings." iconName="play-circle" />
          </SettingsSection>
        {:else if data.apps.length === 0}
          <Callout type="info" title="No organization GitHub App">
            Runners are registered at organization level. Add a GitHub App that belongs to an organization under Sources, and select "Run workflow jobs on your servers" when you register it.
          </Callout>
        {:else if !enabled}
          <SettingsSection id="github-runners-section" title="GitHub Actions runners" description="Each workflow job runs in a new container that is deleted after the job." actions={beta}>
            <Empty size="sm" title="Runners are disabled" description="Enable runners to take GitHub Actions workflow jobs on this server." iconName="play-circle">
              {#snippet contents()}
                {#if data?.is_build_server}
                  <button type="button" class="button" {...{ ishighlighted: '' }} disabled={!canEdit} onclick={toggle}>Enable runners</button>
                {:else}
                  <button type="button" class="button" {...{ ishighlighted: '' }} disabled={!canEdit} onclick={() => (confirmEnable = true)}>Enable runners</button>
                {/if}
              {/snippet}
            </Empty>
          </SettingsSection>
        {:else}
          <form
            class="contents"
            onsubmit={(e) => {
              e.preventDefault();
              save();
            }}
          >
            <UnsavedBar {dirty} {saving} onsave={save} onreset={() => ((form = { ...saved }), (error = null))} />
            <SettingsSection id="github-runners-section" title="GitHub Actions runners" description="Each workflow job runs in a new container that is deleted after the job. Use runs-on: [self-hosted, <your label>] in the workflow." actions={enabledActions}>
              {#if !data.is_build_server}
                <Callout type="warning" title="Runners share this server with your resources" class="mb-4">
                  Workflow jobs use the same CPU, memory, and disk as the resources on this server. A heavy job can slow down or stop them. Use a server with the Builds only role for runners when you can.
                </Callout>
              {/if}
              {#if form.dockerMode === 'dind'}
                <div>
                  <Callout type="warning" title="Use only for trusted repositories">
                    In the privileged Docker mode, a malicious workflow can take control of this server. Use Isolated Docker (Sysbox) or No Docker to keep jobs away from the server. Every repository of the organization can use these runners, public ones too.
                  </Callout>
                </div>
              {/if}
              {#if form.allowPullRequests === 'true'}
                <div class="mt-4">
                  <Callout type="warning" title="Pull request jobs run on this server">
                    In a public repository, anyone can open a pull request and change its workflow. Turn on approval for fork pull request workflows in the organization's Actions settings on GitHub, and do not use the privileged Docker mode.
                  </Callout>
                </div>
              {/if}

              {#if selectedApp && selectedApp.missing.length > 0}
                <Callout type="danger" title="The GitHub App is not ready" class="mt-4">
                  Update the App on GitHub, then click Refetch on the App's Permissions page. Missing:
                  <ul class="mt-1 list-disc pl-4">
                    {#each selectedApp.missing as requirement}
                      <li>{requirement}</li>
                    {/each}
                  </ul>
                  <div class="mt-2 flex flex-wrap gap-2">
                    <a href={permissionsPath(selectedApp)} class="button" target="_blank" rel="noopener noreferrer">
                      Update on GitHub
                      {@render externalLink()}
                    </a>
                    <a href="/source/github/{selectedApp.uuid}/permissions" class="button" use:link>Open App permissions</a>
                  </div>
                </Callout>
              {/if}

              <div class="mt-4 grid gap-4 lg:grid-cols-2">
                <Listbox id="githubAppId" label="GitHub App" required disabled={!canEdit} bind:value={form.app} options={data.apps.map((a) => ({ value: String(a.id), label: `${a.name} (${a.organization})` }))} helper="Organization GitHub App that receives the Workflow job webhook events." error={error?.field('github_app_id')} />
                <FormInput id="labels" label="Labels" required disabled={!canEdit} placeholder="coolify" helper="Comma-separated custom labels. Coolify also registers self-hosted and linux. Jobs must ask for at least one custom label." bind:value={form.labels} error={error?.field('labels')} />
                {#if data.is_build_server}
                  <Listbox id="isDedicated" label="Application builds" disabled={!canEdit} bind:value={form.isDedicated} options={[{ value: 'false', label: 'Also build applications' }, { value: 'true', label: 'Dedicated to runners' }]} helper="Dedicated servers are not used for application builds while runners are enabled." />
                {/if}
                <Listbox id="allowPullRequests" label="Pull request jobs" disabled={!canEdit} bind:value={form.allowPullRequests} options={[{ value: 'false', label: 'Refuse pull request jobs' }, { value: 'true', label: 'Run pull request jobs' }]} helper="Runners fail jobs that a pull request started (pull_request, pull_request_target, and review events) before any step runs. Push, schedule, and manual jobs always run. Applies to runners that start after you save." />
              </div>
            </SettingsSection>

            <SettingsSection id="github-runners-resources-section" title="Resources" description="Limits apply to the runner container and to its Docker sidecar.">
              <div class="grid gap-4 lg:grid-cols-3">
                <FormInput id="maxRunners" type="number" min="1" max="32" label="Parallel runners" required disabled={!canEdit} helper="Jobs wait in the queue when all runners are busy." bind:value={form.maxRunners} error={error?.field('max_runners')} />
                <FormInput id="cpuLimit" label="CPU limit" placeholder="2" disabled={!canEdit} helper="Number of CPUs for each runner. Empty means no limit." bind:value={form.cpuLimit} error={error?.field('cpu_limit')} />
                <FormInput id="memoryLimit" label="Memory limit" placeholder="4g" disabled={!canEdit} helper="Memory for each runner, for example 4g. Empty means no limit." bind:value={form.memoryLimit} error={error?.field('memory_limit')} />
                <Listbox
                  id="dockerMode"
                  label="Docker in jobs"
                  disabled={!canEdit}
                  bind:value={form.dockerMode}
                  onchange={checkSysbox}
                  options={[
                    { value: 'dind', label: 'Docker (privileged, full host access)' },
                    { value: 'sysbox', label: 'Isolated Docker (Sysbox)' },
                    { value: 'none', label: 'No Docker (isolated, no container actions)' },
                  ]}
                  helper="Docker: each job gets a private Docker daemon. The daemon runs privileged, so a job can get root access to this server. Use it only for trusted repositories.<br><br>Isolated Docker: each job gets a private Docker daemon in an unprivileged Sysbox container. Everything that needs Docker works, and root in the job has no rights on this server. Needs Sysbox on the server.<br><br>No Docker: jobs run in an unprivileged container. JavaScript actions, composite actions, and shell steps work. Docker container actions, services, container jobs, and docker commands fail.<br><br>The host Docker socket is never shared."
                  error={error?.field('docker_mode')}
                />
                <FormInput id="runnerImage" label="Runner image" disabled={!canEdit} placeholder={RUNNER_IMAGE} helper="Empty uses the latest official runner image, which Coolify pulls for every runner. Set a tag, for example ghcr.io/actions/actions-runner:2.337.0, to pin a version, or use a custom image based on the official one to add tools. GitHub stops sending jobs to runners that are more than 30 days old." bind:value={form.runnerImage} error={error?.field('runner_image')} />
              </div>
              {#if form.dockerMode === 'sysbox'}
                <div class="mt-4">
                  {#if sysbox === true}
                    <Callout type="success" title="Sysbox is installed">Jobs get a private Docker daemon in an unprivileged container.</Callout>
                  {:else if sysbox === false}
                    <Callout type="warning" title="Sysbox is not installed">
                      Isolated Docker needs the Sysbox runtime. Coolify installs Sysbox {SYSBOX_VERSION} from the official package (Debian and Ubuntu, kernel 5.12 or newer). Docker reloads its configuration, and running containers are not restarted.
                      {#if canEdit}
                        <div class="mt-3">
                          <button type="button" class="button" onclick={installSysbox}>Install Sysbox</button>
                        </div>
                      {/if}
                    </Callout>
                  {:else}
                    <Callout type="info" title="Checking Sysbox">Coolify checks if Sysbox is installed on this server.</Callout>
                  {/if}
                </div>
              {/if}
            </SettingsSection>

            <SettingsSection id="github-runners-timeouts-section" title="Timeouts" description="All values are in minutes.">
              <div class="grid gap-4 lg:grid-cols-3">
                <FormInput id="capacityWaitTimeout" type="number" min="1" max="1440" label="Queue wait" required disabled={!canEdit} helper="A job that waits longer for a free runner is dropped." bind:value={form.capacityWaitTimeout} error={error?.field('capacity_wait_timeout')} />
                <FormInput id="idleTimeout" type="number" min="1" max="1440" label="Idle runner" required disabled={!canEdit} helper="A runner that gets no job in this time is removed." bind:value={form.idleTimeout} error={error?.field('idle_timeout')} />
                <FormInput id="jobTimeout" type="number" min="1" max="7200" label="Job" required disabled={!canEdit} helper="A job that runs longer is stopped." bind:value={form.jobTimeout} error={error?.field('job_timeout')} />
              </div>
            </SettingsSection>
          </form>
        {/if}

        {#if data.can_build && enabled}
          <SettingsSection id="github-runners-executions-section" title="Recent runners" description="Queued jobs, active runners, and their results. The list updates every 10 seconds." flush>
            <div>
              {#each executions as execution (execution.uuid)}
                <div class="flex items-center gap-4 border-b border-neutral-200 px-4 py-3 last:border-b-0 dark:border-white/[0.08]">
                  <StatusBadge label={statusLabel(execution.status)} type={statusType(execution)} />
                  <div class="min-w-0 flex-1">
                    <p class="truncate text-sm font-medium text-neutral-950 dark:text-fg">
                      {execution.repository_full_name ?? 'Unknown repository'}
                      {#if execution.workflow_name || execution.job_name}
                        <span class="font-normal text-neutral-500 dark:text-fg-dim">· {[execution.workflow_name, execution.job_name].filter(Boolean).join(' / ')}</span>
                      {/if}
                    </p>
                    <p class="mt-0.5 truncate text-xs text-neutral-500 dark:text-fg-dim">
                      {diffForHumans(execution.created_at)}
                      {#if ran(execution)}
                        · ran {ran(execution)}
                      {/if}
                      {#if execution.conclusion}
                        · {execution.conclusion}
                      {/if}
                      {#if execution.error_message}
                        · {execution.error_message}
                      {/if}
                    </p>
                  </div>
                  <div class="flex shrink-0 items-center gap-2">
                    {#if (execution.workflow_job_html_url ?? '').trim().startsWith('https://')}
                      <a href={execution.workflow_job_html_url} target="_blank" rel="noopener noreferrer" class="button">
                        Open
                        {@render externalLink()}
                      </a>
                    {/if}
                    {#if isActive(execution) && canEdit}
                      <button type="button" class="button button-error" onclick={() => (cancelling = execution.id)}>Cancel</button>
                    {/if}
                  </div>
                </div>
              {:else}
                <Empty size="sm" title="No runners yet" description="Workflow jobs that ask for this server's labels appear here." iconName="servers" />
              {/each}
            </div>
          </SettingsSection>
        {/if}
      </div>

      {#if confirmEnable}
        <ConfirmModal
          title="Enable runners on this server?"
          confirmWithText={false}
          step2ButtonText="Enable runners"
          warningMessage="This server also runs your resources. We do not recommend GitHub Actions runners here: workflow jobs use the same CPU, memory, and disk, and a heavy job can slow down or stop your resources. Use a server with the Builds only role for runners when you can."
          actions={['Workflow jobs of the GitHub organization will run on this server next to your resources.']}
          onconfirm={toggle}
          onclose={() => (confirmEnable = false)}
        />
      {/if}
      {#if cancelling !== null}
        {@const id = cancelling}
        <ConfirmModal
          title="Cancel this runner?"
          actions={['The runner will be stopped and removed.', 'A running workflow job fails.']}
          confirmWithText={false}
          step2ButtonText="Cancel Runner"
          onconfirm={() => cancel(id)}
          onclose={() => (cancelling = null)}
        />
      {/if}
    {/if}
  {/snippet}
</ServerFrame>
