<script lang="ts">
  // Port of upstream livewire/dashboard/traffic-analytics.blade.php with Dashboard\TrafficAnalytics.
  // Rendered only when a server of the team has traffic analytics on. `#[Lazy]` loads it when its
  // placeholder `<div class="contents">` enters the viewport. That element has no box, so upstream's
  // `x-intersect` does not fire in Chrome and the widget stays empty there; this observes the same element.
  import { onMount } from 'svelte';
  import Reicon from '../Reicon.svelte';
  import Sparkline from '../analytics/Sparkline.svelte';
  import { compactNumber, formatBytes, numberFormat } from '../../lib/analyticsApi';
  import { loadApexCharts } from '../../lib/charts';
  import { fetchDashboardTraffic, type DashboardTraffic } from '../../lib/metricsApi.svelte';
  import { link } from '../../lib/router.svelte';

  const chartId = 'dashboard-traffic';
  const tabButtonBase = 'relative inline-flex h-7 items-center justify-center rounded-md px-2.5 text-[12px] font-medium transition-colors disabled:cursor-not-allowed disabled:opacity-40';
  const tabButtonActive = 'bg-white text-black shadow-sm ring-1 ring-neutral-200 dark:bg-white/[0.09] dark:text-fg dark:ring-white/[0.08]';
  const tabButtonInactive = 'text-neutral-500 hover:text-black dark:text-fg-faint dark:hover:text-fg';
  const card = 'group flex flex-col bg-white px-4 py-3 transition-colors hover:bg-neutral-50 dark:bg-[color-mix(in_srgb,var(--color-app)_95%,white)] dark:hover:bg-[color-mix(in_srgb,var(--color-app)_93%,white)]';
  const tileLabel = 'text-[11px] font-medium tracking-wide text-neutral-500 uppercase dark:text-fg-dim';
  const tileValue = 'mt-1 text-xl font-semibold text-black tabular-nums dark:text-fg';
  const ranges = [
    { value: '24h', label: '24 hours' },
    { value: '7d', label: '7 days' },
    { value: '30d', label: '30 days' },
  ];

  let data = $state<DashboardTraffic | null>(null);
  let range = $state('24h');
  let loading = $state<string | null>(null);
  const series = $derived(data?.series ?? []);
  const categories = $derived(series.map((b) => b.bucket));

  async function load(next: string) {
    loading = next;
    try {
      const [answer] = await Promise.all([fetchDashboardTraffic(next), loadApexCharts()]);
      data = answer;
      range = next;
    } catch {
      // A failed summary shows nothing, as when every server's fetch failed.
    } finally {
      loading = null;
    }
  }

  let placeholder: HTMLDivElement;
  onMount(() => {
    const observer = new IntersectionObserver((entries) => {
      if (entries.some((entry) => entry.isIntersecting)) {
        observer.disconnect();
        void load('24h');
      }
    });
    observer.observe(placeholder);
    return () => observer.disconnect();
  });
</script>

<div bind:this={placeholder} class="contents">
  {#if data?.overview}
    {@const overview = data.overview}
    <section class="mb-0! min-w-0">
      <div class="mb-3 flex items-end justify-between gap-4">
        <div>
          <h2 class="text-[14px]! leading-5! font-semibold! text-black dark:text-fg">Traffic analytics</h2>
          <p class="mt-0.5 text-[11px] text-neutral-500 dark:text-fg-faint">Team-wide request volume across servers with traffic analytics enabled</p>
        </div>

        <div class="flex items-center gap-2">
          <div class="inline-flex items-center gap-0.5 rounded-lg bg-neutral-100 p-1 dark:bg-white/[0.04]">
            {#each ranges as option (option.value)}
              <button type="button" disabled={loading !== null} onclick={() => load(option.value)} class="{tabButtonBase} {range === option.value ? tabButtonActive : tabButtonInactive}">
                <span class={loading === option.value ? 'invisible' : ''}>{option.label}</span>
                {#if loading === option.value}
                  <span class="absolute inline-flex items-center justify-center gap-2 text-[13px] text-neutral-500 dark:text-fg-dim" role="status" aria-live="polite" aria-label="Loading analytics">
                    <svg class="loading-indicator shrink-0 animate-spin size-3" viewBox="0 0 24 24" fill="none" aria-hidden="true">
                      <circle class="opacity-20" cx="12" cy="12" r="9" stroke="currentColor" stroke-width="2" />
                      <path class="opacity-80" d="M21 12a9 9 0 0 0-9-9" stroke="currentColor" stroke-width="2" stroke-linecap="round" />
                    </svg>
                  </span>
                {/if}
              </button>
            {/each}
          </div>
          <a
            href="/analytics"
            use:link
            class="group inline-flex h-7 shrink-0 items-center gap-1.5 rounded-md border border-neutral-200 bg-white px-2.5 text-[12px] font-medium text-neutral-600 transition-[color,background-color,transform] duration-100 ease-out hover:bg-neutral-100 hover:text-black active:scale-[0.97] dark:border-white/[0.08] dark:bg-white/[0.06] dark:text-fg-dim dark:hover:bg-white/[0.1] dark:hover:text-fg"
          >
            Open analytics
            <Reicon name="arrow-right" class="size-3 opacity-70 transition-transform duration-150 ease-out group-hover:translate-x-0.5" />
          </a>
        </div>
      </div>

      <div class="grid grid-cols-1 gap-px overflow-hidden rounded-xl border border-neutral-200 bg-neutral-200 sm:grid-cols-2 lg:grid-cols-4 dark:border-white/[0.08] dark:bg-white/[0.07]">
        <a href="/analytics" use:link class={card}>
          <span class={tileLabel}>Requests</span>
          <span class={tileValue} title="{numberFormat(overview.requests)} requests">{compactNumber(overview.requests)}</span>
          <div class="mt-auto pt-3">
            <Sparkline id="{chartId}-spark-requests" values={series.map((b) => b.requests)} {categories} colorVar="--chart-status-3xx" label="Requests" />
          </div>
        </a>
        <a href="/analytics" use:link class={card}>
          <span class="flex items-center {tileLabel}">
            Unique visitors
            {#if data.uniquesApproximate}
              <span
                title="Summed across {data.serverCount} servers; visitors seen on multiple servers may be double-counted."
                class="ml-1.5 inline-flex items-center rounded-full bg-amber-100 px-1.5 py-0.5 text-[9px] font-medium tracking-wide text-amber-700 uppercase dark:bg-amber-500/10 dark:text-amber-400">~ approximate</span>
            {/if}
          </span>
          <span class={tileValue} title="{numberFormat(overview.uniqueVisitors)} unique visitors">{compactNumber(overview.uniqueVisitors)}</span>
          <div class="mt-auto pt-3">
            <Sparkline id="{chartId}-spark-visitors" values={series.map((b) => b.uniqueVisitors)} {categories} colorVar="--chart-status-2xx" label="Visitors" />
          </div>
        </a>
        <a href="/analytics" use:link class={card}>
          <span class={tileLabel}>Bandwidth</span>
          <span class={tileValue}>{formatBytes(data.bandwidthBytes ?? 0)}</span>
          <div class="mt-auto pt-3">
            <Sparkline id="{chartId}-spark-bandwidth" values={series.map((b) => b.bytesIn + b.bytesOut)} {categories} colorVar="--chart-spark-bandwidth" label="Bandwidth" format="bytes" />
          </div>
        </a>
        <a href="/analytics" use:link class={card}>
          <span class={tileLabel}>Error rate</span>
          <span class={tileValue}>{data.errorRate ?? 0}%</span>
          <div class="mt-auto pt-3">
            <Sparkline id="{chartId}-spark-errors" values={series.map((b) => b.s4xx + b.s5xx)} {categories} colorVar="--chart-status-5xx" label="Errors" />
          </div>
        </a>
      </div>
    </section>
  {/if}
</div>
