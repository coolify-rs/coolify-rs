<script lang="ts">
  // Port of upstream livewire/activity-monitor.blade.php (fullHeight, no
  // header) for a database start: the log of the activity, following the end
  // unless the reader scrolled up. `activity` is null while waiting for it.
  import type { DatabaseState } from '../lib/api';

  let { activity }: { activity: DatabaseState['activity'] } = $props();

  let log: HTMLDivElement | undefined = $state();
  let follow = true;

  const running = $derived(!!activity && (activity.status === 'queued' || activity.status === 'in_progress'));
  const status = $derived(!activity ? null : running ? 'running' : activity.status === 'finished' ? 'finished' : 'failed');
  // RunRemoteProcess::decodeOutput
  const output = $derived(
    activity
      ? [...activity.entries]
          .sort((a, b) => a.order - b.order)
          .map((e) => e.output)
          .join('')
          // Blade's text goes through the HTML parser, which turns carriage returns into line feeds.
          .replace(/\r\n?/g, '\n')
      : '',
  );

  $effect(() => {
    void output;
    if (log && follow) queueMicrotask(() => log && (log.scrollTop = log.scrollHeight));
  });

  function onscroll() {
    if (!log) return;
    follow = log.scrollTop + log.clientHeight >= log.scrollHeight - 5;
  }
</script>

<div class="flex h-full min-h-0 min-w-0 max-w-full flex-col">
  <div data-activity-status={status} class="h-full flex flex-col overflow-hidden">
    {#if activity}
      <div
        bind:this={log}
        {onscroll}
        class="activity-monitor-log flex flex-col w-full min-w-0 max-w-full px-4 py-2 overflow-y-auto bg-white border border-solid rounded-sm dark:text-white dark:bg-coolgray-100 scrollbar border-neutral-300 dark:border-coolgray-300 flex-1 min-h-0"
      >
        <pre class="font-logs min-w-0 max-w-full whitespace-pre-wrap wrap-anywhere">{output}</pre>
      </div>
    {:else}
      <div class="flex justify-start">
        <div class="inline-flex items-center justify-center gap-2 text-[13px] text-neutral-500 dark:text-fg-dim" role="status" aria-live="polite">
          <svg class="loading-indicator shrink-0 animate-spin size-4" viewBox="0 0 24 24" fill="none" aria-hidden="true">
            <circle class="opacity-20" cx="12" cy="12" r="9" stroke="currentColor" stroke-width="2" />
            <path class="opacity-80" d="M21 12a9 9 0 0 0-9-9" stroke="currentColor" stroke-width="2" stroke-linecap="round" />
          </svg>
          <span>Waiting for the process to start...</span>
        </div>
      </div>
    {/if}
  </div>
</div>
