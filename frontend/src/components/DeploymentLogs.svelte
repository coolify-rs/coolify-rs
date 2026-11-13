<script lang="ts">
  import Loading from './Loading.svelte';
  // Port of the logs panel of upstream livewire/project/application/deployment/show.blade.php
  // (+ Deployment\Show) with livewire/project/application/deployment-navbar.blade.php:
  // polling every 2 s while the deployment runs, follow mode, fullscreen,
  // find-in-logs, timestamps and debug toggles, export, and cancel.
  import { untrack } from 'svelte';
  import Reicon from './Reicon.svelte';
  import Dropdown from './Dropdown.svelte';
  import { appUi, ApiError, type DeploymentDetail, type DeploymentLine } from '../lib/api';
  import { logTime } from '../lib/format';
  import { notify } from '../lib/toast.svelte';
  import Callout from './Callout.svelte';
  import { link } from '../lib/router.svelte';
  import GateStatus from './imageScan/GateStatus.svelte';

  let {
    uuid,
    dep,
    initial,
    canUpdate,
    timezone,
    sourceUrl,
    vulnerabilitiesUrl,
    onfinished,
  }: {
    uuid: string;
    dep: string;
    initial: DeploymentDetail;
    canUpdate: boolean;
    timezone: string;
    /** The Source page of the application (`route('project.application.source')`). */
    sourceUrl?: string;
    /** The Security page of the application (`route('project.application.vulnerabilities')`). */
    vulnerabilitiesUrl?: string;
    onfinished?: () => void;
  } = $props();

  let detail = $state<DeploymentDetail>(untrack(() => initial));
  let fullscreen = $state(false);
  let alwaysScroll = $state(untrack(() => initial.is_keep_alive));
  let followManuallyDisabled = false;
  let showTimestamps = $state(true);
  let searchQuery = $state('');
  let debouncedQuery = $state('');
  let togglingDebug = $state(false);
  let downloadingAll = $state(false);
  let container: HTMLDivElement | undefined = $state();
  let searchInput: HTMLInputElement | undefined = $state();
  let isScrolling = false;
  let lastTouchY = 0;

  const keepAlive = $derived(detail.is_keep_alive);
  const status = $derived(detail.status);
  const imageScanGate = $derived(detail.image_scan_gate ?? null);
  // A deployment the image scan gate blocked shows as "Blocked".
  const statusLabel = $derived(imageScanGate?.blocked ? 'Blocked' : status === 'in_progress' ? 'In progress' : headline(status));
  const statusDot = $derived(
    imageScanGate?.blocked
      ? 'bg-red-500'
      : status === 'finished'
      ? 'bg-emerald-500'
      : status === 'failed'
        ? 'bg-red-500'
        : status === 'queued' || status === 'in_progress'
          ? 'bg-warning motion-safe:animate-pulse'
          : 'bg-neutral-400 dark:bg-neutral-500',
  );
  const findShortcutLabel = typeof navigator !== 'undefined' && /Mac|iPhone|iPad/.test(navigator.platform) ? '⌘F' : 'Ctrl+F';

  /** `deployAnyway()`: queue the blocked deployment again with the gate bypassed, then open it. */
  async function deployAnyway() {
    try {
      const r = await appUi.deployAnyway(uuid, dep);
      notify.success(r.message);
      // `redirectRoute(..., 'project.application.deployment.show')`: the page of the new deployment.
      window.location.assign(window.location.pathname.replace(/[^/]+$/, encodeURIComponent(r.deployment_uuid)));
    } catch (e) {
      notify.failure(e instanceof ApiError ? e.message : String(e));
    }
  }

  function headline(s: string): string {
    return s
      .split(/[-_\s]+/)
      .filter(Boolean)
      .map((w) => w[0].toUpperCase() + w.slice(1))
      .join(' ');
  }

  type Line = { ts: string; text: string; content: string; line: DeploymentLine };
  const lines = $derived<Line[]>(
    detail.lines.map((l) => {
      const ts = logTime(l.timestamp, timezone);
      const text = (l.command ? '[CMD]: ' : '') + l.text.trim();
      return { ts, text, content: `${ts} ${text}`, line: l };
    }),
  );
  const query = $derived(debouncedQuery.trim().toLowerCase());
  const matchCount = $derived(query ? lines.filter((l) => l.content.toLowerCase().includes(query)).length : 0);

  let searchTimer: ReturnType<typeof setTimeout> | undefined;
  $effect(() => {
    const q = searchQuery;
    clearTimeout(searchTimer);
    searchTimer = setTimeout(() => (debouncedQuery = q), 300);
  });

  function parts(text: string): { t: string; hit: boolean }[] {
    if (!query) return [{ t: text, hit: false }];
    const out: { t: string; hit: boolean }[] = [];
    const lower = text.toLowerCase();
    let last = 0;
    let i = lower.indexOf(query);
    while (i !== -1) {
      if (i > last) out.push({ t: text.slice(last, i), hit: false });
      out.push({ t: text.slice(i, i + query.length), hit: true });
      last = i + query.length;
      i = lower.indexOf(query, last);
    }
    if (last < text.length) out.push({ t: text.slice(last), hit: false });
    return out;
  }

  // `polling()`: refresh every 2 s while the deployment runs.
  $effect(() => {
    if (!keepAlive) return;
    const t = setInterval(async () => {
      try {
        const fresh = await appUi.deployment(uuid, dep);
        const wasAlive = detail.is_keep_alive;
        detail = fresh;
        if (wasAlive && !fresh.is_keep_alive) {
          onfinished?.();
          setTimeout(() => stopScroll(), 500);
        }
      } catch {
        // Keep the last lines on a failed poll.
      }
    }, 2000);
    return () => clearInterval(t);
  });

  function scrollToBottom() {
    if (!container) return;
    isScrolling = true;
    container.scrollTop = container.scrollHeight;
    requestAnimationFrame(() => (isScrolling = false));
  }
  function stopScroll() {
    scrollToBottom();
    alwaysScroll = false;
  }
  // The follow loop: scroll to the bottom every 250 ms while following.
  $effect(() => {
    if (!alwaysScroll) return;
    let raf = 0;
    const t = setInterval(() => (raf = requestAnimationFrame(scrollToBottom)), 250);
    scrollToBottom();
    return () => {
      clearInterval(t);
      cancelAnimationFrame(raf);
    };
  });
  $effect(() => {
    void lines;
    if (untrack(() => alwaysScroll)) queueMicrotask(scrollToBottom);
  });

  function toggleScroll() {
    alwaysScroll = !alwaysScroll;
    followManuallyDisabled = !alwaysScroll;
  }
  let scrollDebounce: ReturnType<typeof setTimeout> | undefined;
  function handleScroll(e: Event) {
    if (isScrolling) return;
    const el = e.currentTarget as HTMLDivElement;
    clearTimeout(scrollDebounce);
    scrollDebounce = setTimeout(() => {
      const distance = el.scrollHeight - el.scrollTop - el.clientHeight;
      if (!alwaysScroll && !followManuallyDisabled && distance <= 10) alwaysScroll = true;
    }, 150);
  }
  function handleFindShortcut(e: KeyboardEvent) {
    const isFind = (e.ctrlKey || e.metaKey) && !e.altKey && !e.shiftKey && e.key?.toLowerCase() === 'f';
    if (!isFind || !searchInput) return;
    const active = document.activeElement as HTMLElement | null;
    if (active === searchInput) return;
    const root = searchInput.closest('.logs-viewer');
    if (!root?.contains(active) && (active?.isContentEditable || active?.matches('input, textarea, select'))) return;
    e.preventDefault();
    searchInput.focus();
    searchInput.select();
  }

  async function toggleDebug() {
    togglingDebug = true;
    try {
      await appUi.toggleDebug(uuid);
      detail = await appUi.deployment(uuid, dep);
    } catch (e) {
      notify.error(e instanceof ApiError ? e.message : String(e));
    } finally {
      togglingDebug = false;
    }
  }

  async function cancel() {
    try {
      await appUi.cancel(uuid, dep);
      detail = await appUi.deployment(uuid, dep);
    } catch (e) {
      notify.error(e instanceof ApiError ? e.message : String(e));
    }
  }

  // `DeploymentNavbar::force_start`.
  async function forceStart() {
    try {
      const { started } = await appUi.forceStart(uuid, dep);
      detail = await appUi.deployment(uuid, dep);
      if (!started) notify.info('This deployment is no longer queued, so it was not started again.');
    } catch (e) {
      notify.error(e instanceof ApiError ? e.message : String(e));
    }
  }

  // `exportLogs(showTimestamps, searchQuery)`.
  function exportLogs(): string {
    const q = searchQuery.trim().toLowerCase();
    return detail.lines
      .map((l) => {
        const text = (l.command ? '[CMD]: ' : '') + l.text.trim();
        return showTimestamps ? `${logTime(l.timestamp, timezone)} ${text}` : text;
      })
      .filter((l) => !q || l.toLowerCase().includes(q))
      .join('\n');
  }
  function save(content: string, name: string) {
    const blob = new Blob([content], { type: 'text/plain' });
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    a.download = name;
    a.click();
    URL.revokeObjectURL(url);
  }
  const stamp = () => new Date().toISOString().slice(0, 19).replace(/[T:]/g, '-');
  function copyLogs() {
    const content = exportLogs();
    if (!content) return;
    if (!navigator.clipboard?.writeText) {
      notify.error('Clipboard is not available. Please use HTTPS or localhost.');
      return;
    }
    navigator.clipboard
      .writeText(content)
      .then(() => notify.ok('Logs copied to clipboard.'))
      .catch(() => notify.error('Failed to copy logs to clipboard.'));
  }
  function downloadLogs() {
    const content = exportLogs();
    if (!content) return;
    save(content + '\n', `deployment-${detail.deployment_uuid}-${stamp()}.txt`);
  }
  async function downloadAllLogs() {
    downloadingAll = true;
    try {
      const all = await appUi.deployment(uuid, dep, true);
      const content = all.lines
        .map((l) => `${logTime(l.timestamp, timezone)} ${l.hidden ? '[DEBUG] ' : ''}${l.command ? '[CMD]: ' : ''}${l.text.trim()}`)
        .join('\n');
      save(content, `deployment-${detail.deployment_uuid}-all-logs-${stamp()}.txt`);
      notify.ok('All logs downloaded.');
    } catch (e) {
      notify.error(e instanceof ApiError ? e.message : String(e));
    } finally {
      downloadingAll = false;
    }
  }
</script>

<svelte:window onkeydown={handleFindShortcut} />

<div
  class="flex h-[calc(100dvh-8rem)] min-h-[32rem] w-full min-w-0 flex-col overflow-hidden xl:h-[32rem] xl:min-h-0 xl:flex-none 2xl:h-[calc(100dvh-10rem)] 2xl:min-h-[32rem] {fullscreen
    ? ''
    : '2xl:sticky 2xl:top-4'}"
>
  <div id="screen" class={fullscreen ? 'fullscreen flex flex-col' : 'mt-2 flex flex-1 min-h-0 flex-col overflow-hidden lg:mt-0'}>
    {#if detail.host_key_changed && sourceUrl}
      <Callout type="danger" title="The SSH host key of the Git host changed" class="mb-2 shrink-0">
        The Git host sent an SSH host key that is not the saved or published key. This can be an attack. Ask the administrator of the Git host if the key changed. Then fetch the current key, compare its fingerprint with the fingerprint from the administrator, and accept it only if they are the same.
        <a href="{sourceUrl}#ssh-host-keys" use:link class="font-medium underline underline-offset-2">Open SSH host keys</a>
      </Callout>
    {/if}
    <div
      class="logs-viewer flex min-h-0 w-full flex-col overflow-hidden bg-white text-neutral-800 dark:bg-log dark:text-neutral-100 {fullscreen
        ? 'h-full'
        : 'flex-1 rounded-xl border shadow-sm'}"
    >
      <div class="runtime-log-toolbar logs-viewer-toolbar">
        <div class="logs-viewer-toolbar-controls">
          <div class="logs-viewer-primary">
            <div class="logs-viewer-actions">
              {#if keepAlive}
                <button title="Follow Logs" class="logs-viewer-btn {alwaysScroll ? 'logs-viewer-btn-active' : ''}" onclick={toggleScroll}>
                  <svg class="size-4" viewBox="0 0 24 24" xmlns="http://www.w3.org/2000/svg">
                    <path fill="none" stroke="currentColor" stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M12 5v14m4-4l-4 4m-4-4l4 4" />
                  </svg>
                </button>
              {/if}
              {#if !fullscreen}
                <button title="Fullscreen" class="logs-viewer-btn" onclick={() => (fullscreen = true)}>
                  <svg class="size-4" viewBox="0 0 24 24" xmlns="http://www.w3.org/2000/svg">
                    <g fill="none">
                      <path
                        fill="currentColor"
                        d="M9.793 12.793a1 1 0 0 1 1.497 1.32l-.083.094L6.414 19H9a1 1 0 0 1 .117 1.993L9 21H4a1 1 0 0 1-.993-.883L3 20v-5a1 1 0 0 1 1.993-.117L5 15v2.586l4.793-4.793ZM20 3a1 1 0 0 1 .993.883L21 4v5a1 1 0 0 1-1.993.117L19 9V6.414l-4.793 4.793a1 1 0 0 1-1.497-1.32l.083-.094L17.586 5H15a1 1 0 0 1-.117-1.993L15 3h5Z"
                      />
                    </g>
                  </svg>
                </button>
              {:else}
                <button title="Minimize" class="logs-viewer-btn" onclick={() => (fullscreen = false)}>
                  <svg class="size-4" viewBox="0 0 24 24" xmlns="http://www.w3.org/2000/svg">
                    <path
                      fill="none"
                      stroke="currentColor"
                      stroke-linecap="round"
                      stroke-linejoin="round"
                      stroke-width="2"
                      d="M6 14h4m0 0v4m0-4l-6 6m14-10h-4m0 0V6m0 4l6-6"
                    />
                  </svg>
                </button>
              {/if}
            </div>
            {#if !imageScanGate?.blocked}
              <span class="flex shrink-0 items-center gap-1.5 text-xs font-medium text-neutral-600 dark:text-fg-dim">
                {#if status === 'in_progress'}
                  <Loading compact />
                {:else}
                  <span class="size-2 shrink-0 rounded-full {statusDot}"></span>
                {/if}
                {statusLabel}
              </span>
            {/if}
            {#if imageScanGate}
              <GateStatus
                class="shrink-0"
                result={imageScanGate.result}
                vulnerabilitiesUrl={vulnerabilitiesUrl ?? null}
                showDeployAnyway
                canDeployAnyway={imageScanGate.can_deploy_anyway}
                action={imageScanGate.action}
                ondeployanyway={deployAnyway}
              />
            {/if}
          </div>
          <div class="logs-viewer-end">
            <div class="logs-viewer-deployment-actions {status === 'queued' || status === 'in_progress' ? '' : 'hidden'}">
              {#if status === 'queued' || status === 'in_progress'}
                <div class="flex flex-wrap items-center justify-end gap-2">
                  {#if status === 'queued'}
                    <button type="button" class="button logs-viewer-deployment-btn" onclick={forceStart}>Force start</button>
                  {/if}
                  <button type="button" class="button button-error logs-viewer-deployment-btn logs-viewer-cancel-btn" onclick={cancel}>Cancel deployment</button>
                </div>
              {/if}
            </div>
            <div class="logs-viewer-search relative">
              <Reicon
                name="search"
                class="pointer-events-none absolute top-1/2 left-2.5 z-10 size-3.5 -translate-y-1/2 text-neutral-400 dark:text-fg-faint"
              />
              <input
                type="search"
                bind:this={searchInput}
                bind:value={searchQuery}
                placeholder="Find in logs"
                aria-label="Find in logs"
                aria-keyshortcuts={findShortcutLabel === '⌘F' ? 'Meta+F' : 'Control+F'}
                class="h-8! w-full rounded-lg! border-neutral-200! bg-white! py-0! pr-8! pl-8! text-[12px]! shadow-none! placeholder:text-neutral-400 focus:border-accent! focus:ring-0! dark:border-white/[0.08]! dark:bg-white/[0.035]! dark:text-fg! dark:placeholder:text-fg-faint"
              />
              {#if !searchQuery}
                <kbd
                  aria-hidden="true"
                  class="pointer-events-none absolute top-1/2 right-2 z-10 -translate-y-1/2 rounded border border-neutral-200 px-1 font-sans text-[10px] leading-4 text-neutral-400 dark:border-white/[0.08] dark:text-fg-faint"
                  >{findShortcutLabel}</kbd
                >
              {:else}
                <button
                  type="button"
                  onclick={() => (searchQuery = '')}
                  class="absolute top-1/2 right-2 z-10 flex size-5 -translate-y-1/2 items-center justify-center rounded text-neutral-400 transition-colors hover:bg-neutral-100 hover:text-black dark:text-fg-faint dark:hover:bg-white/[0.07] dark:hover:text-fg"
                  aria-label="Clear search"
                >
                  <Reicon name="x" class="size-3" />
                </button>
              {/if}
            </div>
            <div class="runtime-log-menus">
              <Dropdown panelClass="runtime-log-menu min-w-48! max-h-none!" role="menu">
                {#snippet trigger({ open })}
                  <button type="button" title="View Options" class="button" aria-haspopup="menu" aria-expanded={open}>
                    <svg class="size-3.5 shrink-0" xmlns="http://www.w3.org/2000/svg" fill="none" viewBox="0 0 24 24" stroke-width="1.5" stroke="currentColor">
                      <path
                        stroke-linecap="round"
                        stroke-linejoin="round"
                        d="M10.5 6h9.75M10.5 6a1.5 1.5 0 1 1-3 0m3 0a1.5 1.5 0 1 0-3 0M3.75 6H7.5m3 12h9.75m-9.75 0a1.5 1.5 0 0 1-3 0m3 0a1.5 1.5 0 0 0-3 0m-3.75 0H7.5m9-6h3.75m-3.75 0a1.5 1.5 0 0 1-3 0m3 0a1.5 1.5 0 0 0-3 0m-9.75 0h9.75"
                      />
                    </svg>
                    <span>View</span>
                    <Reicon name="chevron-down" class="size-3 transition-transform {open ? 'rotate-180' : ''}" />
                  </button>
                {/snippet}
                {#snippet children()}
                  <div>
                    <p class="runtime-log-menu-label">Display</p>
                    <button type="button" class="listbox-option" role="menuitemcheckbox" aria-checked={showTimestamps} onclick={() => (showTimestamps = !showTimestamps)}>
                      <span class="flex-1 text-left">Timestamps</span>
                      {#if showTimestamps}<span>✓</span>{/if}
                    </button>
                    {#if canUpdate}
                      <button type="button" class="listbox-option" role="menuitemcheckbox" aria-checked={detail.debug} onclick={toggleDebug}>
                        <span class="flex-1 text-left">Debug logs</span>
                        {#if togglingDebug}
                          <svg class="loading-indicator size-3.5 animate-spin" fill="none" viewBox="0 0 24 24" aria-label="Loading debug logs...">
                            <circle class="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" stroke-width="4"></circle>
                            <path
                              class="opacity-75"
                              fill="currentColor"
                              d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4zm2 5.291A7.962 7.962 0 014 12H0c0 3.042 1.135 5.824 3 7.938l3-2.647z"
                            ></path>
                          </svg>
                        {:else if detail.debug}
                          <span>✓</span>
                        {/if}
                      </button>
                    {/if}
                  </div>
                {/snippet}
              </Dropdown>
              <Dropdown panelClass="runtime-log-menu min-w-52!" role="menu">
                {#snippet trigger({ open })}
                  <button type="button" title="Copy or Download Logs" class="button" aria-haspopup="menu" aria-expanded={open}>
                    <svg class="size-3.5 shrink-0" xmlns="http://www.w3.org/2000/svg" fill="none" viewBox="0 0 24 24" stroke-width="1.5" stroke="currentColor">
                      <path
                        stroke-linecap="round"
                        stroke-linejoin="round"
                        d="M3 16.5v2.25A2.25 2.25 0 0 0 5.25 21h13.5A2.25 2.25 0 0 0 21 18.75V16.5M16.5 12 12 16.5m0 0L7.5 12m4.5 4.5V3"
                      />
                    </svg>
                    <span>Export</span>
                    <Reicon name="chevron-down" class="size-3 transition-transform {open ? 'rotate-180' : ''}" />
                  </button>
                {/snippet}
                {#snippet children({ close })}
                  <div>
                    <p class="px-2 py-1.5 text-xs text-neutral-500 dark:text-fg-dim">Copied and downloaded logs hide secrets.</p>
                    <button
                      type="button"
                      role="menuitem"
                      class="listbox-option"
                      onclick={() => {
                        copyLogs();
                        close();
                      }}
                    >
                      <span class="flex-1 text-left">Copy visible logs</span>
                    </button>
                    <button
                      type="button"
                      role="menuitem"
                      class="listbox-option"
                      onclick={() => {
                        downloadLogs();
                        close();
                      }}
                    >
                      <span class="flex-1 text-left">Download visible logs</span>
                    </button>
                    {#if canUpdate}
                      <button
                        type="button"
                        role="menuitem"
                        disabled={downloadingAll}
                        class="listbox-option {downloadingAll ? 'opacity-50 cursor-not-allowed' : ''}"
                        onclick={() => downloadAllLogs().finally(close)}
                      >
                        {#if !downloadingAll}
                          <span class="flex-1 text-left">Download full log (with debug)</span>
                        {:else}
                          <span class="flex items-center gap-2">
                            <svg class="w-4 h-4 animate-spin" xmlns="http://www.w3.org/2000/svg" fill="none" viewBox="0 0 24 24">
                              <circle class="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" stroke-width="4"></circle>
                              <path
                                class="opacity-75"
                                fill="currentColor"
                                d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4zm2 5.291A7.962 7.962 0 014 12H0c0 3.042 1.135 5.824 3 7.938l3-2.647z"
                              ></path>
                            </svg>
                            Downloading...
                          </span>
                        {/if}
                      </button>
                    {/if}
                  </div>
                {/snippet}
              </Dropdown>
            </div>
            <div class="logs-viewer-meta">
              {#if searchQuery.trim()}
                <span class="text-xs text-neutral-500 whitespace-nowrap">{matchCount} matches</span>
              {/if}
            </div>
          </div>
        </div>
      </div>
      <!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_static_element_interactions -->
      <div
        id="logsContainer"
        bind:this={container}
        onscroll={handleScroll}
        onwheel={(e) => alwaysScroll && e.deltaY < 0 && (alwaysScroll = false)}
        ontouchstart={(e) => (lastTouchY = e.touches[0].clientY)}
        ontouchmove={(e) => {
          const y = e.touches[0].clientY;
          if (alwaysScroll && y > lastTouchY) alwaysScroll = false;
          lastTouchY = y;
        }}
        onkeydown={(e) => alwaysScroll && ['ArrowUp', 'PageUp', 'Home'].includes(e.key) && (alwaysScroll = false)}
        tabindex="0"
        class="logs-viewer-viewport flex min-h-40 flex-1 flex-col overflow-y-auto overflow-x-hidden scrollbar"
      >
        <div id="logs" class="flex min-w-0 flex-col font-logs text-[11px] leading-relaxed sm:text-xs">
          {#if query && matchCount === 0}
            <div class="py-2 text-neutral-500">No matches found.</div>
          {/if}
          {#each lines as l, i (i)}
            {@const matches = !query || l.content.toLowerCase().includes(query)}
            <div data-log-line class="log-line logs-viewer-line" class:mt-2={l.line.command} class:hidden={!matches}>
              {#if showTimestamps}<span class="logs-viewer-timestamp">{l.ts}</span>{/if}
              <span
                class="{l.line.hidden ? 'text-success dark:text-warning ' : ''}{l.line.stderr ? 'text-red-500 ' : ''}{l.line.command ? 'font-bold ' : ''}logs-viewer-line-text"
                >{#each parts(l.text) as p, j (j)}{#if p.hit}<span class="log-highlight">{p.t}</span>{:else}{p.t}{/if}{/each}</span
              >
            </div>
          {:else}
            <span class="mb-2 font-logs text-neutral-400">No logs yet.</span>
          {/each}
        </div>
      </div>
    </div>
  </div>
</div>
