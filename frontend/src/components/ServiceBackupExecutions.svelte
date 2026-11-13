<script lang="ts">
  // Port of upstream livewire/project/service/backup-executions.blade.php (Project/Service/BackupExecutions).
  import { untrack } from 'svelte';
  import Empty from './Empty.svelte';
  import Modal from './Modal.svelte';
  import Pager from './Pager.svelte';
  import Reicon from './Reicon.svelte';
  import SettingsSection from './SettingsSection.svelte';
  import SortHeader from './SortHeader.svelte';
  import StatusBadge from './StatusBadge.svelte';
  import { diffForHumans, parseTimestamp } from '../lib/format';
  import { downloadBackup, formatBytes, volumeBackupsUi, type ServiceExecution } from '../lib/volumeBackupsApi';

  let { service }: { service: string } = $props();

  let executions = $state<ServiceExecution[]>([]);
  let total = $state(0);
  let page = $state(1);
  let perPage = $state(10);
  let timezone = $state<string | null>(null);
  let s3Downloads = $state(false);
  let loaded = $state(false);
  let selected = $state<ServiceExecution | null>(null);
  // `$sortBy`: a column and a direction, the newest first by default.
  let sortBy = $state('started_desc');
  let sorting = $state(false);
  let copied = $state(false);

  /** `sortExecutions`: sorts on the server and goes back to the first page. */
  async function sortExecutions(value: string) {
    sortBy = value;
    page = 1;
    sorting = true;
    try {
      await load();
    } finally {
      sorting = false;
    }
  }

  /** `x-copy-button`: copies the backup path. */
  function copyPath(path: string) {
    navigator.clipboard?.writeText(path).catch(() => {});
    copied = true;
    setTimeout(() => (copied = false), 1000);
  }

  async function load() {
    const r = await volumeBackupsUi.serviceExecutions(service, page, perPage, sortBy);
    executions = r.executions;
    total = r.total;
    page = r.page;
    timezone = r.server_timezone;
    s3Downloads = r.s3_downloads;
    loaded = true;
  }
  $effect(() => {
    void page;
    void perPage;
    untrack(() => load());
    // `wire:poll.5000ms` on the first page.
    const poll = setInterval(() => page === 1 && load().catch(() => {}), 5000);
    return () => clearInterval(poll);
  });

  /** `formatDateInServerTimezone`: `Y-m-d H:i:s T`. */
  function serverDate(ts: string | null | undefined): string {
    const d = parseTimestamp(ts);
    if (!d) return '';
    try {
      const parts = new Intl.DateTimeFormat('en-CA', {
        timeZone: timezone || 'UTC',
        hour12: false,
        year: 'numeric',
        month: '2-digit',
        day: '2-digit',
        hour: '2-digit',
        minute: '2-digit',
        second: '2-digit',
        timeZoneName: 'short',
      }).formatToParts(d);
      const part = (t: string) => parts.find((p) => p.type === t)?.value ?? '';
      const hour = part('hour') === '24' ? '00' : part('hour');
      return `${part('year')}-${part('month')}-${part('day')} ${hour}:${part('minute')}:${part('second')} ${part('timeZoneName')}`;
    } catch {
      return d.toISOString();
    }
  }
  const headline = (s: string) => s.replace(/[_-]+/g, ' ').replace(/\b\w/g, (c) => c.toUpperCase());
  const statusType = (s: string): 'success' | 'error' | 'warning' | 'neutral' => (s === 'success' ? 'success' : s === 'failed' ? 'error' : s === 'running' ? 'warning' : 'neutral');
</script>

{#if selected}
  {@const e = selected}
  <Modal title="Backup execution" isLarge focusFirstField={false} onclose={() => (selected = null)}>
    <div class="flex flex-col gap-5">
      <div class="grid gap-4 sm:grid-cols-2 lg:grid-cols-4">
        <div><p class="text-xs text-neutral-500 dark:text-fg-dim">Target</p><p class="mt-1 text-sm font-medium">{e.target}</p></div>
        <div><p class="text-xs text-neutral-500 dark:text-fg-dim">Status</p><p class="mt-1 text-sm font-medium">{headline(e.status)}</p></div>
        <div><p class="text-xs text-neutral-500 dark:text-fg-dim">Started</p><p class="mt-1 text-sm font-medium" title={diffForHumans(e.started_at)}>{serverDate(e.started_at)}</p></div>
        <div><p class="text-xs text-neutral-500 dark:text-fg-dim">Size</p><p class="mt-1 text-sm font-medium">{formatBytes(e.size)}</p></div>
      </div>
      {#if e.filename}
        <div>
          <p class="text-xs text-neutral-500 dark:text-fg-dim">Backup path</p>
          <div class="mt-1 flex items-center gap-2 rounded-md bg-neutral-100 p-3 dark:bg-black/20">
            <code class="min-w-0 flex-1 overflow-x-auto text-xs">{e.filename}</code>
            <button type="button" title="Copy backup path" aria-label="Copy backup path" class="icon-button group shrink-0" onclick={() => copyPath(e.filename ?? '')}>
              <span class="inline-flex transition-transform duration-150 ease-out group-active:scale-75">
                {#if copied}<Reicon name="check" class="size-3.5 text-success" />{:else}<Reicon name="copy" class="size-3.5" />{/if}
              </span>
            </button>
          </div>
        </div>
      {/if}
      {#if e.message}
        <div><p class="text-xs text-neutral-500 dark:text-fg-dim">Output</p><pre class="mt-1 max-h-80 overflow-auto rounded-md bg-neutral-100 p-3 font-mono text-xs whitespace-pre-wrap dark:bg-black/20">{e.message}</pre></div>
      {/if}
    </div>
  </Modal>
{/if}

<SettingsSection title="Executions" description="Review backup runs across every database and storage target in this service." flush>
  {#if loaded}
    {#if !executions.length}
      <Empty size="sm" title="No backup executions" description="Execution history appears here after a backup schedule runs." iconName="browser-terminal" />
    {:else}
      <div class="data-table relative w-full overflow-x-auto">
        <div class="flex min-w-[64rem] flex-col">
        <div class="data-table-header service-backup-table-grid">
          <SortHeader server loading={sorting} label="Target" asc="target_asc" desc="target_desc" current={sortBy} onsort={sortExecutions} />
          <SortHeader server loading={sorting} label="Type" asc="type_asc" desc="type_desc" current={sortBy} onsort={sortExecutions} />
          <SortHeader server loading={sorting} label="Schedule" asc="schedule_asc" desc="schedule_desc" current={sortBy} onsort={sortExecutions} />
          <SortHeader server loading={sorting} label="Status" asc="status_asc" desc="status_desc" current={sortBy} onsort={sortExecutions} />
          <SortHeader server loading={sorting} label="Started" asc="started_asc" desc="started_desc" first="desc" current={sortBy} onsort={sortExecutions} />
          <SortHeader server loading={sorting} label="Size" asc="size_asc" desc="size_desc" first="desc" current={sortBy} onsort={sortExecutions} />
          <span class="text-right">Actions</span>
        </div>
        {#each executions as e (e.id)}
          <div
            onclick={() => (selected = e)}
            onkeydown={(ev) => ev.key === 'Enter' && (selected = e)}
            role="button"
            tabindex="0"
            class="data-table-row service-backup-table-grid cursor-pointer text-left text-[13px] text-neutral-700 dark:text-fg-dim"
          >
            <span class="flex min-w-0 items-center gap-2 font-medium text-neutral-950 dark:text-fg">
              <span class="truncate" title={e.target}>{e.target}</span>
              {#if e.s3_tooltip}
                <span tabindex="0" role="note" data-tooltip={e.s3_tooltip} aria-label={e.s3_tooltip} class="shrink-0 rounded-full bg-neutral-100 px-1.5 py-0.5 text-[10px] font-semibold leading-none text-neutral-600 dark:bg-white/[0.06] dark:text-fg-dim">S3</span>
              {/if}
            </span>
            <span>{e.type}</span><span>{e.schedule}</span>
            <span><StatusBadge label={headline(e.status)} type={statusType(e.status)} /></span>
            <span>
              <span tabindex="0" role="note" data-tooltip={serverDate(e.started_at)}>{diffForHumans(e.started_at)}</span>
            </span>
            <span>{formatBytes(e.size)}</span>
            <span class="flex justify-end">
              {#if e.download_url && s3Downloads}
                <button
                  type="button"
                  onclick={(ev) => {
                    ev.stopPropagation();
                    downloadBackup(e.type === 'Database' ? 'database' : 'storage', Number(e.id.split(':')[1]), true);
                  }}
                  class="icon-button shrink-0"
                  title="Download backup"
                  aria-label="Download backup"
                >
                  <Reicon name="upload" class="size-3.5 rotate-180" />
                </button>
              {:else if e.download_url}
                <a href={e.download_url} target="_blank" rel="noopener" onclick={(ev) => ev.stopPropagation()} class="icon-button shrink-0" title="Download backup" aria-label="Download backup">
                  <Reicon name="upload" class="size-3.5 rotate-180" />
                </a>
              {/if}
            </span>
          </div>
        {/each}
        </div>
        {#if total > perPage}
          <Pager {total} bind:page bind:pageSize={perPage} separator="–" sizeSelect={false} />
        {/if}
      </div>
    {/if}
  {/if}
</SettingsSection>
