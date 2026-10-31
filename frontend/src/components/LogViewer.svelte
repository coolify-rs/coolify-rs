<script lang="ts" module>
  /** The server half of `Project\Shared\GetLogs`: what a page passes to `LogViewer`. */
  export type LogRequest = { mode: 'get' | 'copy' | 'all'; lines: number; timestamps: boolean; since?: string };
</script>

<script lang="ts">
  // Port of upstream livewire/project/shared/get-logs.blade.php with
  // resources/js/runtime-logs.js, the log viewer every log page shares.
  import { onDestroy, onMount, tick } from 'svelte';
  import { Virtualizer, elementScroll, measureElement, observeElementOffset, observeElementRect } from '@tanstack/virtual-core';
  import Reicon from './Reicon.svelte';
  import { notify } from '../lib/toast.svelte';
  import {
    ESTIMATED_ROW_HEIGHT,
    ESTIMATED_TEXT_ROW_HEIGHT,
    MAX_CLIENT_LOG_LINES,
    STREAM_INTERVAL_MS,
    createTimestampFormatter,
    downloadText,
    filterLogLines,
    formatLogDetails,
    highlightSegments,
    isExpandableLog,
    isFindShortcut,
    newLinesSince,
    parseLogLines,
    prefersTextView,
    readLogFilters,
    type LogFilters,
    type LogLevel,
    type LogLine,
  } from '../lib/runtimeLogs';

  let {
    fetchLogs,
    containerName = 'logs',
    timezone = 'UTC',
    displayName,
    collapsible = true,
    expandByDefault = false,
    fill = false,
    timestamps = $bindable(true),
    ontimestamps,
  }: {
    fetchLogs: (request: LogRequest) => Promise<string>;
    containerName?: string;
    timezone?: string;
    displayName?: string;
    collapsible?: boolean;
    expandByDefault?: boolean;
    fill?: boolean;
    timestamps?: boolean;
    ontimestamps?: (value: boolean) => void;
  } = $props();

  // Line records stay outside the reactive state, as upstream keeps them outside Alpine's.
  let lines: LogLine[] = [];
  let filtered: LogLine[] = [];
  let lineId = 0;
  const nextId = () => ++lineId;
  let virtualizer: Virtualizer<HTMLElement, Element> | null = null;
  let cleanupVirtualizer: (() => void) | undefined;
  let layoutObserver: ResizeObserver | undefined;
  let streamTimer: ReturnType<typeof setInterval> | undefined;
  let scrollDebounce: ReturnType<typeof setTimeout> | undefined;
  let destroyed = false;
  let isScrolling = false;
  let lastTouchY = 0;
  let followManuallyDisabled = false;
  // svelte-ignore state_referenced_locally
  const formatTimestamp = createTimestampFormatter(timezone);

  const stored = (key: string): string | null => {
    try {
      return localStorage.getItem(key);
    } catch {
      return null;
    }
  };
  const store = (key: string, value: string) => {
    try {
      localStorage.setItem(key, value);
    } catch {
      // A blocked store only loses the preference.
    }
  };

  // svelte-ignore state_referenced_locally
  let expanded = $state(expandByDefault || !collapsible);
  let logsLoaded = $state(false);
  let streamLogs = $state(false);
  let numberOfLines = $state<number | null>(100);
  let allLines = $state(false);
  let loadingAll = $state(false);
  let loadingLines = $state(false);
  let loading = $state(false);
  let fullscreen = $state(false);
  let alwaysScroll = $state(false);
  let colorLogs = $state(stored('coolify-color-logs') === 'true');
  let textView = $state(prefersTextView(stored('coolify-log-text-view')));
  let logFilters = $state<LogFilters>(readLogFilters(stored('coolify-log-filters')));
  let searchInput = $state('');
  let searchQuery = $state('');
  let appliedQuery = $state('');
  let matchCount = $state(0);
  let lineCount = $state(0);
  let expandedLogs = $state<Record<number, boolean>>({});
  let virtualRows = $state<{ key: string | number | bigint; index: number; start: number; line: LogLine }[]>([]);
  let totalSize = $state(0);
  let scrollMargin = $state(0);
  let multipleSources = $state(false);
  let menu = $state<'view' | 'export' | null>(null);
  let panelStyle = $state('position: fixed; min-width: 0; visibility: hidden;');
  let downloadingAllLogs = $state(false);
  let searchDebounce: ReturnType<typeof setTimeout> | undefined;

  let root = $state<HTMLElement>();
  let viewport = $state<HTMLElement>();
  let listEl = $state<HTMLElement>();
  let searchEl = $state<HTMLInputElement>();
  let viewTrigger = $state<HTMLElement>();
  let exportTrigger = $state<HTMLElement>();
  let panelEl = $state<HTMLElement>();

  const findShortcutLabel = /Mac|iPhone|iPad/.test(navigator.platform) ? '⌘F' : 'Ctrl+F';
  const anyFilterOff = $derived(Object.values(logFilters).some((v) => !v));
  const levels: { id: LogLevel; label: string; dot: string }[] = [
    { id: 'error', label: 'Error', dot: 'bg-red-500' },
    { id: 'warning', label: 'Warning', dot: 'bg-yellow-500' },
    { id: 'debug', label: 'Debug', dot: 'bg-purple-500' },
    { id: 'info', label: 'Info', dot: 'bg-blue-500' },
  ];

  const resolveLines = () => (numberOfLines === -1 ? -1 : numberOfLines == null || numberOfLines <= 0 ? 1000 : Math.min(numberOfLines, 50000));

  function measureScrollMargin(): number {
    if (!listEl || !viewport || lineCount === 0) return 0;
    const margin = listEl.getBoundingClientRect().top - viewport.getBoundingClientRect().top + viewport.scrollTop;
    return Number.isFinite(margin) ? Math.max(0, margin) : 0;
  }
  function pinScrollOffset() {
    if (!virtualizer || alwaysScroll || !viewport || viewport.scrollTop !== 0) return;
    if (!virtualizer.scrollOffset && !virtualizer.scrollAdjustments) return;
    virtualizer.scrollOffset = 0;
    virtualizer.scrollAdjustments = 0;
  }
  function syncRows() {
    if (!virtualizer) return;
    virtualRows = virtualizer
      .getVirtualItems()
      .filter((item) => filtered[item.index])
      .map((item) => ({ key: item.key, index: item.index, start: item.start, line: filtered[item.index] }));
    totalSize = virtualizer.getTotalSize();
  }
  function updateVirtualizer() {
    if (!virtualizer) return;
    pinScrollOffset();
    scrollMargin = measureScrollMargin();
    virtualizer.setOptions({
      ...virtualizer.options,
      count: filtered.length,
      scrollMargin,
      getItemKey: (index: number) => filtered[index]?.id ?? index,
    });
    virtualizer._willUpdate();
    syncRows();
  }
  function mountVirtualizer() {
    if (destroyed || virtualizer) return;
    virtualizer = new Virtualizer<HTMLElement, Element>({
      count: 0,
      getScrollElement: () => viewport ?? null,
      estimateSize: () => (textView ? ESTIMATED_TEXT_ROW_HEIGHT : ESTIMATED_ROW_HEIGHT),
      getItemKey: (index: number) => filtered[index]?.id ?? index,
      overscan: 12,
      observeElementRect,
      observeElementOffset,
      scrollToFn: elementScroll,
      // A row can be measured before its text is laid out; caching that 0 hides the first lines.
      measureElement: (element, entry, instance) => {
        const size = measureElement(element, entry, instance);
        if (size > 0) return size;
        const index = instance.indexFromElement(element);
        const key = instance.options.getItemKey(index);
        return instance.itemSizeCache.get(key) ?? instance.options.estimateSize(index);
      },
      onChange: () => {
        pinScrollOffset();
        syncRows();
      },
    });
    cleanupVirtualizer = virtualizer._didMount();
    virtualizer._willUpdate();
    if (window.ResizeObserver) {
      layoutObserver = new ResizeObserver(() => {
        if (measureScrollMargin() !== scrollMargin) updateVirtualizer();
      });
      if (listEl) layoutObserver.observe(listEl);
      if (viewport) layoutObserver.observe(viewport);
    }
    updateVirtualizer();
  }
  function measureRow(el: HTMLElement, logLine: LogLine | null = null) {
    const line = el.querySelector('[data-log-line]');
    const message = line?.querySelector<HTMLElement>('.runtime-log-message');
    if (line && message && !textView) {
      const text = logLine?.text ?? message.textContent ?? '';
      line.toggleAttribute('data-expandable', isExpandableLog(text, message.scrollWidth > message.clientWidth));
    }
    virtualizer?.measureElement(el);
  }
  /** `x-init="$nextTick(() => measureRow($el, row.line))"`. */
  function measured(el: HTMLElement, line: LogLine) {
    tick().then(() => measureRow(el, line));
  }

  async function applyFilters() {
    filtered = filterLogLines(lines, logFilters, searchQuery);
    appliedQuery = searchQuery.trim();
    matchCount = appliedQuery ? filtered.length : 0;
    lineCount = lines.length;
    multipleSources = new Set(filtered.map((line) => line.source).filter(Boolean)).size > 1;
    await tick();
    updateVirtualizer();
    if (alwaysScroll) scrollToBottom();
  }
  function replaceLines(output: string) {
    if (destroyed) return;
    lines = parseLogLines(output, nextId);
    expandedLogs = {};
    applyFilters();
  }
  function appendLines(fresh: LogLine[]) {
    if (fresh.length === 0) return;
    lines = lines.concat(fresh);
    if (lines.length > MAX_CLIENT_LOG_LINES) lines = lines.slice(lines.length - MAX_CLIENT_LOG_LINES);
    applyFilters();
  }
  const failed = (message: string) => () => {
    notify.failure(message);
  };
  async function refresh() {
    if (loading) return;
    loading = true;
    try {
      replaceLines(await fetchLogs({ mode: 'get', lines: resolveLines(), timestamps }));
    } catch (e) {
      failed(e instanceof Error ? e.message : String(e))();
    } finally {
      loading = false;
      tick().then(updateVirtualizer);
    }
  }
  async function refreshLines() {
    if (loading) return;
    loadingLines = true;
    try {
      await refresh();
    } finally {
      loadingLines = false;
    }
  }
  async function showAllLogs() {
    if (loading) return;
    loading = true;
    loadingAll = true;
    try {
      numberOfLines = -1;
      replaceLines(await fetchLogs({ mode: 'get', lines: -1, timestamps }));
      allLines = true;
    } catch (e) {
      failed(e instanceof Error ? e.message : String(e))();
    } finally {
      loading = false;
      loadingAll = false;
    }
  }
  function sinceArgument(): string | null {
    let last: string | null = null;
    for (const line of lines) if (line.ts) last = line.ts;
    return last;
  }
  async function streamTick() {
    if (destroyed || loading || !expanded || document.hidden || !streamLogs) return;
    const since = sinceArgument();
    if (!since) {
      await refresh();
      return;
    }
    loading = true;
    try {
      const output = await fetchLogs({ mode: 'get', lines: resolveLines(), timestamps, since });
      if (destroyed || !streamLogs) return;
      appendLines(newLinesSince(lines, parseLogLines(output, nextId)));
    } catch {
      // The next tick tries again.
    } finally {
      loading = false;
    }
  }
  function visibleText(source: LogLine[]): string {
    const showSource = new Set(source.map((line) => line.source).filter(Boolean)).size > 1;
    return source
      .map((line) => {
        const body = timestamps && line.ts ? `${formatTimestamp(line.ts)} ${line.text}` : line.text;
        return showSource && line.source ? `${line.source} | ${body}` : body;
      })
      .join('\n');
  }
  function toggleLogDetails(id: number, event: MouseEvent | KeyboardEvent) {
    if (textView) return;
    if (window.getSelection()?.toString()) return;
    if (event instanceof KeyboardEvent && !['Enter', ' '].includes(event.key)) return;
    if (event instanceof KeyboardEvent) event.preventDefault();
    if (!expandedLogs[id] && !(event.currentTarget as HTMLElement | null)?.hasAttribute('data-expandable')) return;
    expandedLogs[id] = !expandedLogs[id];
  }
  const isLogExpanded = (id: number) => !textView && expandedLogs[id] === true;
  function toggleLogFilter(level: LogLevel) {
    logFilters[level] = !logFilters[level];
    store('coolify-log-filters', JSON.stringify(logFilters));
    applyFilters();
  }
  function toggleColorLogs() {
    colorLogs = !colorLogs;
    store('coolify-color-logs', String(colorLogs));
  }
  async function toggleTextView() {
    textView = !textView;
    store('coolify-log-text-view', String(textView));
    await tick();
    virtualizer?.measure();
    listEl?.querySelectorAll<HTMLElement>('.runtime-log-virtual-row').forEach((el) => measureRow(el));
    updateVirtualizer();
    if (alwaysScroll) scrollToBottom();
  }
  async function toggleTimestamps() {
    timestamps = !timestamps;
    ontimestamps?.(timestamps);
  }
  async function copyLogs() {
    menu = null;
    let logs: string;
    try {
      logs = await fetchLogs({ mode: 'copy', lines: resolveLines(), timestamps });
    } catch {
      notify.failure('Failed to prepare logs for clipboard.');
      return;
    }
    logs = logs.replace(/^([^\t\n]+)\t/gm, '$1 | ');
    if (!navigator.clipboard?.writeText) {
      notify.failure('Clipboard is not available. Please use HTTPS or localhost.');
      return;
    }
    navigator.clipboard.writeText(logs).then(
      () => notify.success('Logs copied to clipboard.'),
      () => notify.failure('Failed to copy logs to clipboard.'),
    );
  }
  async function downloadLogs() {
    menu = null;
    let logs: string;
    try {
      logs = await fetchLogs({ mode: 'copy', lines: resolveLines(), timestamps });
    } catch {
      notify.failure('Failed to prepare logs for download.');
      return;
    }
    if (!logs) return;
    let id = 0;
    const exported = filterLogLines(parseLogLines(logs, () => ++id), logFilters, searchQuery);
    const stamp = new Date().toISOString().slice(0, 19).replace(/[T:]/g, '-');
    downloadText(visibleText(exported) + '\n', `${containerName}-logs-${stamp}.txt`);
  }
  async function downloadAllLogs() {
    downloadingAllLogs = true;
    try {
      const logs = await fetchLogs({ mode: 'all', lines: -1, timestamps });
      if (!logs) return;
      const stamp = new Date().toISOString().slice(0, 19).replace(/[T:]/g, '-');
      downloadText(logs, `${containerName}-all-logs-${stamp}.txt`);
      notify.success('All logs downloaded.');
    } catch (e) {
      notify.failure(e instanceof Error ? e.message : String(e));
    } finally {
      downloadingAllLogs = false;
      menu = null;
    }
  }
  function makeFullscreen() {
    fullscreen = !fullscreen;
    if (!fullscreen) alwaysScroll = false;
  }
  function ownsFindShortcut(): boolean {
    if (!expanded) return false;
    const active = document.activeElement;
    if (active === searchEl) return false;
    if (root?.contains(active)) return true;
    if (active?.closest('[data-runtime-logs]')) return false;
    if ((active as HTMLElement | null)?.isContentEditable || active?.matches('input, textarea, select')) return false;
    const target = document.querySelector('[data-runtime-logs="fullscreen"]') ?? document.querySelector('[data-runtime-logs="expanded"]');
    return target === root;
  }
  function handleKeyDown(event: KeyboardEvent) {
    if (event.key === 'Escape') menu = null;
    if (event.key === 'Escape' && fullscreen) makeFullscreen();
    if (isFindShortcut(event) && ownsFindShortcut()) {
      event.preventDefault();
      searchEl?.focus();
      searchEl?.select();
    }
  }
  function scrollToBottom() {
    if (destroyed || !virtualizer || filtered.length === 0) return;
    isScrolling = true;
    virtualizer.scrollToIndex(filtered.length - 1, { align: 'end' });
    setTimeout(() => (isScrolling = false), 50);
  }
  function toggleScroll() {
    alwaysScroll = !alwaysScroll;
    followManuallyDisabled = !alwaysScroll;
    updateVirtualizer();
    if (alwaysScroll) scrollToBottom();
  }
  function handleScroll(event: Event) {
    if (isScrolling || destroyed) return;
    clearTimeout(scrollDebounce);
    scrollDebounce = setTimeout(() => {
      if (destroyed) return;
      const el = event.target as HTMLElement;
      const distanceFromBottom = el.scrollHeight - el.scrollTop - el.clientHeight;
      const overflows = el.scrollHeight - el.clientHeight > 10;
      if (!alwaysScroll && !followManuallyDisabled && overflows && el.scrollTop > 0 && distanceFromBottom < 10) {
        alwaysScroll = true;
        updateVirtualizer();
      }
    }, 150);
  }
  function onSearchInput() {
    clearTimeout(searchDebounce);
    searchDebounce = setTimeout(() => {
      searchQuery = searchInput;
      applyFilters();
    }, 300);
  }
  function clearSearch() {
    searchInput = '';
    searchQuery = '';
    applyFilters();
  }

  // `x-table.dropdown`: the panel opens under the trigger, right aligned, or above when there is no room.
  async function toggleMenu(which: 'view' | 'export') {
    if (menu === which) {
      menu = null;
      return;
    }
    panelStyle = 'position: fixed; min-width: 0; visibility: hidden;';
    menu = which;
    await tick();
    updateMenuPosition();
  }
  function updateMenuPosition() {
    const triggerEl = menu === 'view' ? viewTrigger : exportTrigger;
    if (!menu || !triggerEl || !panelEl) return;
    const trigger = triggerEl.getBoundingClientRect();
    const panel = panelEl.getBoundingClientRect();
    const pad = 8;
    const left = Math.max(pad, Math.min(trigger.right - panel.width, window.innerWidth - panel.width - pad));
    const spaceBelow = window.innerHeight - trigger.bottom - pad;
    const top = spaceBelow >= panel.height ? trigger.bottom + 4 : Math.max(pad, trigger.top - panel.height - 4);
    panelStyle = `position: fixed; left: ${left}px; top: ${top}px; min-width: 0;`;
  }
  function outside(e: MouseEvent) {
    if (!menu) return;
    const t = e.target as Node;
    if (viewTrigger?.contains(t) || exportTrigger?.contains(t) || panelEl?.contains(t)) return;
    menu = null;
  }

  onMount(async () => {
    allLines = numberOfLines === -1;
    await tick();
    mountVirtualizer();
    streamTimer = setInterval(streamTick, STREAM_INTERVAL_MS);
    if (expanded) {
      refresh();
      logsLoaded = true;
    }
  });
  onDestroy(() => {
    destroyed = true;
    clearInterval(streamTimer);
    clearTimeout(scrollDebounce);
    clearTimeout(searchDebounce);
    layoutObserver?.disconnect();
    cleanupVirtualizer?.();
    virtualizer = null;
  });
</script>

{#snippet spinner(aria: string, klass = '')}
  <div class="inline-flex items-center justify-center gap-2 text-[13px] text-neutral-500 dark:text-fg-dim {klass}" role="status" aria-live="polite" aria-label={aria}>
    <svg class="loading-indicator shrink-0 animate-spin size-3" viewBox="0 0 24 24" fill="none" aria-hidden="true">
      <circle class="opacity-20" cx="12" cy="12" r="9" stroke="currentColor" stroke-width="2" />
      <path class="opacity-80" d="M21 12a9 9 0 0 0-9-9" stroke="currentColor" stroke-width="2" stroke-linecap="round" />
    </svg>
  </div>
{/snippet}

<svelte:window onkeydown={handleKeyDown} onclick={outside} onresize={updateMenuPosition} onscroll={updateMenuPosition} />

<div class={fill ? 'flex min-h-0 min-w-0 flex-1 flex-col' : ''}>
  <div class="w-full min-w-0 {collapsible ? 'runtime-log-shell' : ''} {fill ? 'runtime-logs-fill flex flex-1 flex-col' : ''}">
    <div id="screen" class={fill ? 'flex min-h-0 flex-1 flex-col' : ''} bind:this={root} data-runtime-logs={fullscreen ? 'fullscreen' : expanded ? 'expanded' : 'collapsed'}>
      {#if collapsible}
        <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
        <div
          class="runtime-log-trigger"
          onclick={() => {
            expanded = !expanded;
            if (expanded && !logsLoaded) {
              refresh();
              logsLoaded = true;
            }
          }}
        >
          <svg class="w-4 h-4 transition-transform {expanded ? 'rotate-90' : ''}" viewBox="0 0 24 24" xmlns="http://www.w3.org/2000/svg">
            <path fill="currentColor" d="M8.59 16.59L13.17 12 8.59 7.41 10 6l6 6-6 6-1.41-1.41z" />
          </svg>
          <h4>{displayName ?? containerName}</h4>
          {#if streamLogs}{@render spinner('Loading')}{/if}
        </div>
      {/if}
      <div
        hidden={!expanded}
        class="runtime-logs-stream {fill ? 'flex min-h-0 w-full flex-1 flex-col' : 'mx-auto w-full'} {fullscreen ? 'fullscreen flex flex-col !overflow-visible' : 'relative'}"
        style={fullscreen ? 'max-height: none !important; height: 100% !important;' : ''}
      >
        <div class="runtime-log-panel {fullscreen ? 'h-full w-full' : fill ? 'flex min-h-0 flex-1 flex-col' : ''}">
          <div class="runtime-log-toolbar logs-viewer-toolbar">
            <div class="logs-viewer-toolbar-controls">
              <div class="logs-viewer-actions">
                <button
                  type="button"
                  onclick={() => (streamLogs = !streamLogs)}
                  title={streamLogs ? 'Stop Streaming' : 'Stream Logs'}
                  class="runtime-log-icon-button {streamLogs ? 'runtime-log-icon-button-active' : ''}"
                >
                  {#if streamLogs}
                    <svg class="size-4" viewBox="0 0 24 24" xmlns="http://www.w3.org/2000/svg" fill="currentColor"><path d="M6 4h4v16H6V4zm8 0h4v16h-4V4z" /></svg>
                  {:else}
                    <svg class="size-4" viewBox="0 0 24 24" xmlns="http://www.w3.org/2000/svg" fill="currentColor"><path d="M8 5v14l11-7L8 5z" /></svg>
                  {/if}
                </button>
                <button type="button" onclick={refresh} title="Refresh Logs" disabled={streamLogs} class="runtime-log-icon-button">
                  <Reicon name="refresh" class="size-3.5" />
                </button>
                <button type="button" title="Follow Logs" class="runtime-log-icon-button {alwaysScroll ? 'runtime-log-icon-button-active' : ''}" onclick={toggleScroll}>
                  <svg class="size-4" viewBox="0 0 24 24" xmlns="http://www.w3.org/2000/svg">
                    <path fill="none" stroke="currentColor" stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M12 5v14m4-4l-4 4m-4-4l4 4" />
                  </svg>
                </button>
                {#if !fullscreen}
                  <button type="button" title="Fullscreen" onclick={makeFullscreen} class="runtime-log-icon-button">
                    <svg class="size-4" viewBox="0 0 24 24" xmlns="http://www.w3.org/2000/svg">
                      <g fill="none">
                        <path fill="currentColor" d="M9.793 12.793a1 1 0 0 1 1.497 1.32l-.083.094L6.414 19H9a1 1 0 0 1 .117 1.993L9 21H4a1 1 0 0 1-.993-.883L3 20v-5a1 1 0 0 1 1.993-.117L5 15v2.586l4.793-4.793ZM20 3a1 1 0 0 1 .993.883L21 4v5a1 1 0 0 1-1.993.117L19 9V6.414l-4.793 4.793a1 1 0 0 1-1.497-1.32l.083-.094L17.586 5H15a1 1 0 0 1-.117-1.993L15 3h5Z" />
                      </g>
                    </svg>
                  </button>
                {:else}
                  <button type="button" title="Minimize" onclick={makeFullscreen} class="runtime-log-icon-button">
                    <svg class="size-4" viewBox="0 0 24 24" xmlns="http://www.w3.org/2000/svg">
                      <path fill="none" stroke="currentColor" stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M6 14h4m0 0v4m0-4l-6 6m14-10h-4m0 0V6m0 4l6-6" />
                    </svg>
                  </button>
                {/if}
              </div>
              <div class="logs-viewer-end runtime-logs-viewer-end">
                <div class="logs-viewer-meta">
                  <form
                    class="logs-viewer-lines runtime-log-lines-group"
                    onsubmit={(e) => {
                      e.preventDefault();
                      refreshLines();
                    }}
                  >
                    <span class="logs-viewer-lines-label relative" aria-busy={loadingLines}>
                      <span class={loadingLines ? 'invisible' : ''}>Lines</span>
                      {#if loadingLines}{@render spinner('Loading logs', 'absolute inset-0 text-inherit!')}{/if}
                    </span>
                    <input
                      type="number"
                      bind:value={numberOfLines}
                      oninput={() => (allLines = Number(numberOfLines) === -1)}
                      placeholder="100"
                      min="-1"
                      max="50000"
                      title="Number of lines (max 50,000; use -1 for all)"
                      readonly={streamLogs}
                      class="input logs-viewer-lines-input"
                    />
                    <button
                      type="button"
                      onclick={showAllLogs}
                      title="Show all logs"
                      aria-pressed={allLines}
                      aria-busy={loadingAll}
                      class="runtime-log-lines-all relative {allLines ? 'runtime-log-lines-all-active' : ''}"
                      disabled={streamLogs}
                    >
                      <span class={loadingAll ? 'invisible' : ''}>All</span>
                      {#if loadingAll}{@render spinner('Loading all logs', 'absolute inset-0 text-inherit!')}{/if}
                    </button>
                  </form>
                </div>
                <div class="logs-viewer-search relative">
                  <Reicon name="search" class="pointer-events-none absolute top-1/2 left-2.5 z-10 size-3.5 -translate-y-1/2 text-neutral-400 dark:text-fg-faint" />
                  <input
                    type="search"
                    bind:this={searchEl}
                    bind:value={searchInput}
                    oninput={onSearchInput}
                    placeholder="Find in logs"
                    aria-label="Find in logs"
                    aria-keyshortcuts={findShortcutLabel === '⌘F' ? 'Meta+F' : 'Control+F'}
                    class="h-8! w-full rounded-lg! border-neutral-200! bg-white! py-0! pr-8! pl-8! text-[12px]! shadow-none! placeholder:text-neutral-400 focus:border-accent! focus:ring-0! dark:border-white/[0.08]! dark:bg-white/[0.035]! dark:text-fg! dark:placeholder:text-fg-faint"
                  />
                  {#if !searchInput}
                    <kbd aria-hidden="true" class="pointer-events-none absolute top-1/2 right-2 z-10 -translate-y-1/2 rounded border border-neutral-200 px-1 font-sans text-[10px] leading-4 text-neutral-400 dark:border-white/[0.08] dark:text-fg-faint">{findShortcutLabel}</kbd>
                  {:else}
                    <button type="button" onclick={clearSearch} class="absolute top-1/2 right-2 z-10 flex size-5 -translate-y-1/2 items-center justify-center rounded text-neutral-400 transition-colors hover:bg-neutral-100 hover:text-black dark:text-fg-faint dark:hover:bg-white/[0.07] dark:hover:text-fg" aria-label="Clear search">
                      <Reicon name="x" class="size-3" />
                    </button>
                  {/if}
                </div>
                <div class="runtime-log-menus">
                  <div class="relative">
                    <div bind:this={viewTrigger}>
                      <button type="button" title="View Options" class="button {anyFilterOff ? 'button-highlighted' : ''}" aria-haspopup="menu" aria-expanded={menu === 'view'} onclick={() => toggleMenu('view')}>
                        <svg class="size-3.5 shrink-0" xmlns="http://www.w3.org/2000/svg" fill="none" viewBox="0 0 24 24" stroke-width="1.5" stroke="currentColor">
                          <path stroke-linecap="round" stroke-linejoin="round" d="M10.5 6h9.75M10.5 6a1.5 1.5 0 1 1-3 0m3 0a1.5 1.5 0 1 0-3 0M3.75 6H7.5m3 12h9.75m-9.75 0a1.5 1.5 0 0 1-3 0m3 0a1.5 1.5 0 0 0-3 0m-3.75 0H7.5m9-6h3.75m-3.75 0a1.5 1.5 0 0 1-3 0m3 0a1.5 1.5 0 0 0-3 0m-9.75 0h9.75" />
                        </svg>
                        <span>View</span>
                        <Reicon name="chevron-down" class="size-3 transition-transform {menu === 'view' ? 'rotate-180' : ''}" />
                      </button>
                    </div>
                    {#if menu === 'view'}
                      <div bind:this={panelEl} style={panelStyle} class="listbox-panel origin-top fixed! right-auto! bottom-auto! z-[90]! mt-0! runtime-log-menu min-w-48! max-h-none!" role="menu">
                        <div>
                          <p class="runtime-log-menu-label">Display</p>
                          <button type="button" class="listbox-option" role="menuitemcheckbox" aria-checked={timestamps} onclick={toggleTimestamps}>
                            <span class="flex-1 text-left">Timestamps</span>
                            {#if timestamps}<span>✓</span>{/if}
                          </button>
                          <button type="button" class="listbox-option" role="menuitemcheckbox" aria-checked={textView} onclick={toggleTextView}>
                            <span class="flex-1 text-left">Text view</span>
                            {#if textView}<span>✓</span>{/if}
                          </button>
                          <button type="button" class="listbox-option" role="menuitemcheckbox" aria-checked={colorLogs} onclick={toggleColorLogs}>
                            <span class="flex-1 text-left">Color by level</span>
                            {#if colorLogs}<span>✓</span>{/if}
                          </button>
                          <p class="runtime-log-menu-label">Log levels</p>
                          {#each levels as level (level.id)}
                            <button type="button" class="listbox-option" role="menuitemcheckbox" aria-checked={logFilters[level.id]} onclick={() => toggleLogFilter(level.id)}>
                              <span class="w-2.5 h-2.5 rounded-full {level.dot}"></span>
                              <span class="flex-1 text-left">{level.label}</span>
                              {#if logFilters[level.id]}<span>✓</span>{/if}
                            </button>
                          {/each}
                        </div>
                      </div>
                    {/if}
                  </div>
                  <div class="relative">
                    <div bind:this={exportTrigger}>
                      <button type="button" title="Copy or Download Logs" class="button" aria-haspopup="menu" aria-expanded={menu === 'export'} onclick={() => toggleMenu('export')}>
                        <svg class="size-3.5 shrink-0" xmlns="http://www.w3.org/2000/svg" fill="none" viewBox="0 0 24 24" stroke-width="1.5" stroke="currentColor">
                          <path stroke-linecap="round" stroke-linejoin="round" d="M3 16.5v2.25A2.25 2.25 0 0 0 5.25 21h13.5A2.25 2.25 0 0 0 21 18.75V16.5M16.5 12 12 16.5m0 0L7.5 12m4.5 4.5V3" />
                        </svg>
                        <span>Export</span>
                        <Reicon name="chevron-down" class="size-3 transition-transform {menu === 'export' ? 'rotate-180' : ''}" />
                      </button>
                    </div>
                    {#if menu === 'export'}
                      <div bind:this={panelEl} style={panelStyle} class="listbox-panel origin-top fixed! right-auto! bottom-auto! z-[90]! mt-0! runtime-log-menu min-w-52!" role="menu">
                        <div>
                          <p class="px-2 py-1.5 text-xs text-neutral-500 dark:text-fg-dim">Copied and downloaded logs hide secrets. The live view shows the raw output.</p>
                          <button type="button" role="menuitem" class="listbox-option" onclick={copyLogs}><span class="flex-1 text-left">Copy logs</span></button>
                          <button type="button" role="menuitem" class="listbox-option" onclick={downloadLogs}><span class="flex-1 text-left">Download logs</span></button>
                          <button type="button" role="menuitem" onclick={downloadAllLogs} disabled={downloadingAllLogs} class="listbox-option {downloadingAllLogs ? 'opacity-50 cursor-not-allowed' : ''}">
                            {#if !downloadingAllLogs}
                              <span class="flex-1 text-left">Download all logs</span>
                            {:else}
                              <span class="flex items-center gap-2">
                                <svg class="w-4 h-4 animate-spin" xmlns="http://www.w3.org/2000/svg" fill="none" viewBox="0 0 24 24">
                                  <circle class="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" stroke-width="4"></circle>
                                  <path class="opacity-75" fill="currentColor" d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4zm2 5.291A7.962 7.962 0 014 12H0c0 3.042 1.135 5.824 3 7.938l3-2.647z"></path>
                                </svg>
                                Downloading...
                              </span>
                            {/if}
                          </button>
                        </div>
                      </div>
                    {/if}
                  </div>
                </div>
              </div>
            </div>
          </div>
          <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
          <div
            id="logsContainer"
            bind:this={viewport}
            onscroll={handleScroll}
            onwheel={(e) => alwaysScroll && e.deltaY < 0 && (alwaysScroll = false)}
            ontouchstart={(e) => (lastTouchY = e.touches[0].clientY)}
            ontouchmove={(e) => {
              if (!alwaysScroll) return;
              const y = e.touches[0].clientY;
              if (y > lastTouchY) alwaysScroll = false;
              lastTouchY = y;
            }}
            onkeydown={(e) => alwaysScroll && ['ArrowUp', 'PageUp', 'Home'].includes(e.key) && (alwaysScroll = false)}
            tabindex="0"
            class="runtime-log-viewport logs-viewer-viewport relative flex min-w-0 w-full flex-col overflow-x-hidden overflow-y-auto scrollbar {fullscreen ? 'flex-1' : fill ? 'min-h-0 flex-1' : 'max-h-[min(40rem,70dvh)] sm:max-h-[40rem]'}"
          >
            {#if loading && lineCount === 0}
              <div class="runtime-log-loading" role="status">
                <div class="inline-flex items-center justify-center gap-2 text-[13px] text-neutral-500 dark:text-fg-dim" role="status" aria-live="polite" aria-label="Loading logs">
                  <svg class="loading-indicator shrink-0 animate-spin size-3" viewBox="0 0 24 24" fill="none" aria-hidden="true">
                    <circle class="opacity-20" cx="12" cy="12" r="9" stroke="currentColor" stroke-width="2" />
                    <path class="opacity-80" d="M21 12a9 9 0 0 0-9-9" stroke="currentColor" stroke-width="2" stroke-linecap="round" />
                  </svg>
                </div>
                <span>Loading logs</span>
              </div>
            {/if}
            {#if !loading && lineCount === 0}
              <div class="runtime-log-empty" role="status">
                <div>
                  <p>No logs yet</p>
                  <span>Logs will appear here when the container produces output.</span>
                </div>
              </div>
            {/if}
            <div
              id="logs"
              style:display={lineCount > 0 ? null : 'none'}
              class="font-logs max-w-full cursor-default text-[11px] leading-relaxed sm:text-xs {!timestamps ? 'runtime-log-without-time' : ''} {textView ? 'runtime-log-text-view' : ''} {alwaysScroll ? 'mt-auto' : ''}"
            >
              {#if !textView}
                <div class="runtime-log-columns" aria-hidden="true">
                  {#if timestamps}<span>Time</span>{/if}
                  <span>Type</span>
                  <span>Message</span>
                </div>
              {/if}
              <div bind:this={listEl} class="runtime-log-virtual-list" style:height="{totalSize}px">
                {#each virtualRows as row (row.key)}
                  <div class="runtime-log-virtual-row" data-index={row.index} style:transform="translateY({row.start - scrollMargin}px)" use:measured={row.line}>
                    <!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_static_element_interactions -->
                    <div
                      data-log-line
                      role={textView ? null : 'button'}
                      tabindex={textView ? null : 0}
                      data-log-level={row.line.level}
                      aria-expanded={isLogExpanded(row.line.id)}
                      onclick={(e) => toggleLogDetails(row.line.id, e)}
                      onkeydown={(e) => toggleLogDetails(row.line.id, e)}
                      class="log-line logs-viewer-line {colorLogs ? 'log-' + row.line.level : ''}"
                    >
                      {#if timestamps && row.line.ts}
                        <span class="logs-viewer-timestamp text-gray-500">{formatTimestamp(row.line.ts)}</span>
                      {/if}
                      <span data-line-text class="logs-viewer-line-text">
                        {#if multipleSources && row.line.source}<span class="runtime-log-source" style:color={row.line.sourceColor}>{row.line.source}</span>{/if}
                        <span class="runtime-log-message">{#each highlightSegments(row.line.text, appliedQuery) as part, i (i)}<span class={part.match ? 'log-highlight' : ''}>{part.text}</span>{/each}</span>
                      </span>
                    </div>
                    {#if isLogExpanded(row.line.id)}
                      <pre class="runtime-log-detail" aria-label="Full log entry">{formatLogDetails(row.line.text)}</pre>
                    {/if}
                  </div>
                {/each}
              </div>
              {#if appliedQuery && matchCount === 0}
                <div class="px-4 py-2 text-gray-500 dark:text-gray-400">No matches found.</div>
              {/if}
            </div>
          </div>
        </div>
      </div>
    </div>
  </div>
</div>
