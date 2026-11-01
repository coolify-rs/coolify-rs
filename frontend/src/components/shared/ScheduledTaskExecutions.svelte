<script lang="ts">
  // Port of upstream livewire/project/shared/scheduled-task/executions.blade.php
  // (Project\Shared\ScheduledTask\Executions).
  import { untrack } from 'svelte';
  import Empty from '../Empty.svelte';
  import Reicon from '../Reicon.svelte';
  import StatusBadge from '../StatusBadge.svelte';
  import { diffForHumans, duration, serverTime } from '../../lib/format';
  import { scheduledTasksUi, type TaskExecution, type TaskOwnerKind } from '../../lib/scheduledTasksApi';

  let { kind, uuid, task, timezone }: { kind: TaskOwnerKind; uuid: string; task: string; timezone: string } = $props();

  const LOGS_PER_PAGE = 100;
  let executions = $state<TaskExecution[]>([]);
  let canRead = $state(false);
  let selectedKey = $state<number | null>(null);
  let currentPage = $state(1);

  async function refresh() {
    const r = await scheduledTasksUi.executions(kind, uuid, task);
    executions = r.executions;
    canRead = r.can_read_output;
  }

  const selected = $derived(executions.find((e) => e.id === selectedKey) ?? null);
  // `isPollingActive`: every second while the selected execution runs.
  const polling = $derived(selected?.status === 'running');

  $effect(() => {
    untrack(() => refresh().catch(() => {}));
    const poll = setInterval(() => refresh().catch(() => {}), 5000);
    return () => clearInterval(poll);
  });
  $effect(() => {
    if (!polling) return;
    const poll = setInterval(() => refresh().catch(() => {}), 1000);
    return () => clearInterval(poll);
  });

  function select(id: number) {
    selectedKey = id === selectedKey ? null : id;
    currentPage = 1;
  }

  const lines = (e: TaskExecution) => (e.message ?? '').split('\n');
  // `getLogLinesProperty()`
  function logLines(e: TaskExecution): string[] {
    if (!canRead) return ['Hidden (only admins can view)'];
    if (!e.message) return ['Waiting for task output...'];
    return lines(e).slice(0, currentPage * LOGS_PER_PAGE);
  }
  const hasMore = (e: TaskExecution) => !!e.message && canRead && lines(e).length > currentPage * LOGS_PER_PAGE;

  // `downloadLogs()`: `task-execution-{id}.log`.
  function download(e: TaskExecution) {
    const url = URL.createObjectURL(new Blob([e.message ?? ''], { type: 'text/plain' }));
    const a = document.createElement('a');
    a.href = url;
    a.download = `task-execution-${e.id}.log`;
    a.click();
    URL.revokeObjectURL(url);
  }

  const statusType = (s: string) => (s === 'success' ? 'success' : s === 'failed' ? 'error' : s === 'running' ? 'warning' : 'neutral');
  const statusText = (s: string) => (s === 'success' ? 'Success' : s === 'running' ? 'Running' : s === 'failed' ? 'Failed' : (s ?? '').charAt(0).toUpperCase() + (s ?? '').slice(1));
</script>

<div>
  {#each executions as execution (execution.id)}
    <div class="border-b border-neutral-200 last:border-b-0 dark:border-white/[0.07]">
      <button type="button" onclick={() => select(execution.id)} class="flex w-full items-center gap-3 px-4 py-3.5 text-left transition-colors hover:bg-neutral-50 dark:hover:bg-white/[0.025]">
        <div class="flex size-8 shrink-0 items-center justify-center rounded-lg bg-neutral-100 text-neutral-500 ring-1 ring-neutral-200 dark:bg-white/[0.05] dark:text-fg-dim dark:ring-white/[0.07]">
          {#if execution.status === 'running'}
            <svg class="size-4 animate-spin text-coollabs dark:text-warning" xmlns="http://www.w3.org/2000/svg" fill="none" viewBox="0 0 24 24" aria-hidden="true">
              <circle class="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" stroke-width="4"></circle>
              <path class="opacity-75" fill="currentColor" d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4zm2 5.291A7.962 7.962 0 014 12H0c0 3.042 1.135 5.824 3 7.938l3-2.647z"></path>
            </svg>
          {:else}
            <Reicon name="browser-terminal" class="size-4" />
          {/if}
        </div>
        <div class="min-w-0 flex-1">
          <p class="text-[13px] font-medium text-black dark:text-fg">{serverTime(execution.created_at, timezone)}</p>
          <p class="mt-0.5 text-xs text-neutral-500 dark:text-fg-dim">
            {#if execution.status === 'running'}
              In progress
            {:else}
              {duration(execution.created_at, execution.finished_at)} · finished {diffForHumans(execution.finished_at)}
            {/if}
          </p>
        </div>
        <StatusBadge label={statusText(execution.status)} type={statusType(execution.status)} />
      </button>

      {#if execution.id === selectedKey}
        <div class="border-t border-neutral-200 bg-neutral-50 p-4 dark:border-white/[0.07] dark:bg-black/10">
          <pre class="max-h-[600px] overflow-y-auto whitespace-pre-wrap rounded-lg bg-neutral-950 p-4 font-mono text-xs leading-5 text-neutral-200 ring-1 ring-black/20 scrollbar">{logLines(execution).join('\n')}</pre>
          <div class="mt-3 flex flex-wrap gap-2">
            {#if hasMore(execution)}
              <button type="button" class="button" onclick={() => currentPage++}>Load more</button>
              <button type="button" class="button" onclick={() => (currentPage = Math.ceil(lines(execution).length / LOGS_PER_PAGE))}>Load all</button>
            {/if}
            {#if execution.message && canRead}
              <button type="button" class="button" onclick={() => download(execution)}>Download logs</button>
            {/if}
          </div>
        </div>
      {/if}
    </div>
  {:else}
    <Empty size="sm" title="No executions yet" description="Run this task now or wait for its next scheduled execution." iconName="browser-terminal" />
  {/each}
</div>
