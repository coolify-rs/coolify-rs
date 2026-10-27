<script lang="ts">
  // Port of upstream livewire/server/validate-and-install.blade.php without
  // installation: coolify-rs validates but does not install prerequisites or
  // Docker, so the "ask" step (which warns about installing) is not shown.
  import Reicon from './Reicon.svelte';
  import CheckpointItem from './CheckpointItem.svelte';
  import type { ValidationProgress } from '../lib/api';
  import { sanitizeHtml } from '../lib/sanitize';

  let {
    progress,
    canRetry,
    onretry,
    onclose,
  }: { progress: ValidationProgress | null; canRetry: boolean; onretry: () => void; onclose: () => void } = $props();

  type Status = 'pending' | 'running' | 'success' | 'error';
  const resolve = (value: boolean | null, visible: boolean, hasError: boolean): Status =>
    !visible ? 'pending' : value === true ? 'success' : hasError ? 'error' : 'running';

  const p = $derived(
    progress ?? {
      running: true,
      uptime: null,
      supported_os_type: null,
      prerequisites_installed: null,
      docker_installed: null,
      docker_compose_installed: null,
      docker_version: null,
      error: null,
    },
  );
  const error = $derived(!!p.error);
  const showOs = $derived(!!p.uptime);
  const showPrerequisites = $derived(!!(p.uptime && p.supported_os_type));
  const showDocker = $derived(!!(p.uptime && p.supported_os_type && p.prerequisites_installed));
  const showVersion = $derived(showDocker && !!p.docker_compose_installed);
  const complete = $derived(
    !!(p.uptime && p.supported_os_type && p.prerequisites_installed && p.docker_installed && p.docker_compose_installed && p.docker_version),
  );
  const checkpoints = $derived([
    {
      title: 'Server is reachable',
      description: 'Verify SSH connectivity and key-based authentication',
      status: resolve(p.uptime, true, error && !p.uptime),
    },
    {
      title: 'Supported OS type',
      description: 'Confirm a supported Linux distribution',
      status: resolve(p.supported_os_type, showOs, error && showOs && !p.supported_os_type),
    },
    {
      title: 'Prerequisites are installed',
      description: 'Install required system packages when missing',
      status: resolve(p.prerequisites_installed, showPrerequisites, error && showPrerequisites && !p.prerequisites_installed),
    },
    {
      title: 'Docker is installed',
      description: 'Install or detect Docker Engine',
      status: resolve(p.docker_installed, showDocker, error && showDocker && !p.docker_installed),
    },
    {
      title: 'Docker Compose is installed',
      description: 'Install or detect Docker Compose',
      status: resolve(p.docker_compose_installed, showDocker, error && showDocker && !p.docker_compose_installed),
    },
    {
      title: 'Minimum Docker version',
      description: 'Require Docker Engine 24 or newer',
      status: resolve(p.docker_version, showVersion, error && showVersion && p.docker_version === false),
    },
  ]);
</script>

<div class="flex h-full min-h-0 flex-col gap-4 overflow-y-auto scrollbar">
  <div data-validation-checkpoints class="shrink-0 overflow-hidden rounded-[10px] border border-neutral-200 dark:border-white/[0.08]">
    <div class="border-b border-neutral-200 px-4 py-2.5 dark:border-white/[0.08]">
      <h3 class="text-[13px] font-medium text-neutral-600 dark:text-fg-dim">Validation checkpoints</h3>
    </div>
    <div class="divide-y divide-neutral-200 dark:divide-white/[0.07]">
      {#each checkpoints as c (c.title)}
        <CheckpointItem title={c.title} description={c.description} status={c.status} />
      {/each}
    </div>
  </div>

  {#if complete}
    <div class="mt-auto flex shrink-0 items-center justify-between gap-3 rounded-[10px] border border-emerald-500/20 bg-emerald-500/[0.06] px-4 py-3">
      <div class="flex items-center gap-2 text-[13px] font-medium text-emerald-700 dark:text-emerald-300">
        <Reicon name="check-circle" class="size-4 shrink-0" />
        Validation complete
      </div>
      <button type="button" class="button" onclick={onclose}>Close</button>
    </div>
  {/if}

  {#if p.error}
    <div class="rounded-[10px] border border-red-500/20 bg-red-500/[0.06] px-4 py-3 text-[13px] leading-5 text-red-700 dark:text-red-300">
      <div class="mb-1 flex items-center gap-2 text-[12px] font-semibold uppercase tracking-[0.06em]">
        <Reicon name="alert-circle" class="size-3.5 shrink-0" />
        Validation failed
      </div>
      <div class="font-mono text-[12px] leading-5 whitespace-pre-line">{@html sanitizeHtml(p.error)}</div>
    </div>
    <button type="button" class="button" disabled={!canRetry || p.running} onclick={onretry}>
      <Reicon name="refresh" class="size-3.5" />
      Retry validation
    </button>
  {/if}
</div>
