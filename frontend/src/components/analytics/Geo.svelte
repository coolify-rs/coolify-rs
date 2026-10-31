<script lang="ts">
  // Port of upstream livewire/traffic/_geo.blade.php.
  import { onMount } from 'svelte';
  import Empty from '../Empty.svelte';
  import SettingsSection from '../SettingsSection.svelte';
  import { mountTrafficGlobe } from '../../lib/trafficGlobe';
  import { compactNumber, countryFlagUrl, formatBytes, numberFormat, round1, type CityRow, type CountryRow, type GeoMarkers } from '../../lib/analyticsApi';

  let {
    chartId,
    countries,
    cities,
    attribution,
    geoInit,
    geo,
  }: { chartId: string; countries: CountryRow[]; cities: CityRow[]; attribution: string | null; geoInit: GeoMarkers; geo: GeoMarkers } = $props();

  // Storage can be blocked (sandboxed iframe, privacy mode, full quota); the switch then works without saving.
  function storedView(): 'countries' | 'cities' {
    try {
      return localStorage.getItem('traffic-geo-view') === 'cities' ? 'cities' : 'countries';
    } catch {
      return 'countries';
    }
  }
  let view = $state(storedView());
  function setView(next: 'countries' | 'cities') {
    view = next;
    try {
      localStorage.setItem('traffic-geo-view', next);
    } catch {
      // The switch works without saving.
    }
  }

  const hasData = $derived(countries.length > 0);
  const maxRequests = $derived(Math.max(1, ...countries.map((r) => r.requests)));
  const maxCityRequests = $derived(Math.max(1, ...cities.map((r) => r.requests)));
  const globeId = $derived(`${chartId}-globe`);
  const viewButton = 'h-7 rounded-md px-2.5 text-[12px] font-medium transition-colors';
  const viewButtonActive = 'bg-white text-black shadow-sm ring-1 ring-neutral-200 dark:bg-white/[0.09] dark:text-fg dark:ring-white/[0.08]';
  const viewButtonInactive = 'text-neutral-500 hover:text-black dark:text-fg-faint dark:hover:text-fg';
  const listClass = 'max-h-[320px] overflow-y-auto pr-1 scrollbar-thin scrollbar-track-transparent scrollbar-thumb-neutral-300 dark:scrollbar-thumb-white/10';
  const rowClass = 'group flex min-h-10 items-center gap-3 rounded-md px-2 py-1.5 transition-colors hover:bg-neutral-50 dark:hover:bg-white/[0.03]';

  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  let controller: any = null;
  let canvas: HTMLCanvasElement | undefined = $state();
  const isDark = () => document.documentElement.classList.contains('dark');
  /** City markers in the Cities view; country markers when there are no cities. */
  const markersOf = (markers: GeoMarkers, v: string) => (v === 'cities' && markers.cities.length ? markers.cities : markers.countries);

  onMount(() => () => {
    controller?.destroy();
    controller = null;
  });
  let current = $state<GeoMarkers | null>(null);
  let firstGeo = true;
  $effect(() => {
    const next = geo;
    if (firstGeo) {
      firstGeo = false;
      current = geoInit;
      return;
    }
    current = next;
  });
  $effect(() => {
    const markers = current;
    const v = view;
    if (!canvas || !markers) return;
    if (!controller) {
      controller = mountTrafficGlobe(canvas, markersOf(markers, v), isDark());
      return;
    }
    controller.update(markersOf(markers, v), isDark());
  });
  function hideImage(e: Event) {
    (e.currentTarget as HTMLImageElement).style.visibility = 'hidden';
  }
</script>

{#snippet flag(code: string)}
  {@const url = code ? countryFlagUrl(code) : null}
  {#if url}
    <img loading="lazy" src={url} srcset="{countryFlagUrl(code, '48x36')} 2x" alt="" width="20" height="15" class="h-[15px] w-5 shrink-0 rounded-[2px] object-cover ring-1 ring-black/5 dark:ring-white/10" onerror={hideImage} />
  {:else}
    <span class="w-5 shrink-0 text-center text-[14px] leading-none" aria-hidden="true">🌐</span>
  {/if}
{/snippet}

{#snippet volume(requests: number, bytesOut: number, max: number)}
  <div class="hidden h-1 w-16 shrink-0 overflow-hidden rounded-full bg-neutral-100 sm:block dark:bg-white/[0.06]">
    <div class="h-full rounded-full bg-[var(--chart-status-3xx)]" style="width: {Math.min(100, round1((requests / max) * 100))}%;"></div>
  </div>
  <span class="w-12 shrink-0 text-right text-[12px] font-medium tabular-nums text-black dark:text-fg" title="{numberFormat(requests)} requests">{compactNumber(requests)}</span>
  <span class="hidden w-16 shrink-0 text-right text-[11px] tabular-nums text-neutral-400 sm:inline dark:text-fg-faint">{formatBytes(bytesOut)}</span>
{/snippet}

<SettingsSection id="analytics-country-section" title="Locations" description="Request volume by visitor country and city for the selected range." flush>
  {#snippet actions()}
    {#if hasData}
      <div class="inline-flex items-center gap-0.5 rounded-lg bg-neutral-100 p-1 dark:bg-white/[0.04]" role="group" aria-label="Location detail">
        {#each [['countries', 'Countries'], ['cities', 'Cities']] as [viewKey, viewLabel] (viewKey)}
          <button type="button" onclick={() => setView(viewKey as 'countries' | 'cities')} aria-pressed={view === viewKey ? 'true' : 'false'} class="{viewButton} {view === viewKey ? viewButtonActive : viewButtonInactive}">{viewLabel}</button>
        {/each}
      </div>
    {/if}
  {/snippet}

  <div class="flex flex-col">
    {#if !hasData}
      <Empty size="sm" title="No data" description="No country data for the selected range." iconName="network" />
    {:else}
      <div class="grid grid-cols-1 items-start gap-4 border-b border-neutral-200 p-4 lg:grid-cols-[2fr_3fr] dark:border-white/[0.07]">
        <div class="flex items-center justify-center">
          <div class="relative aspect-square w-full max-w-[320px]">
            <canvas bind:this={canvas} id={globeId} class="h-full w-full [touch-action:none]" style="width: 100%; height: 100%;"></canvas>
          </div>
        </div>

        <div class="min-w-0">
          {#if view === 'countries'}
            <div class={listClass}>
              {#each countries as row (row.value)}
                <div
                  role="presentation"
                  onmouseenter={() => row.value !== '' && controller?.focus(row.value)}
                  onmouseleave={() => row.value !== '' && controller?.resume()}
                  class={rowClass}
                >
                  {@render flag(row.value)}
                  <span class="min-w-0 flex-1 truncate text-[12px] text-black dark:text-fg">{row.value === '' ? 'Unknown' : row.name}</span>
                  {@render volume(row.requests, row.bytesOut, maxRequests)}
                </div>
              {/each}
            </div>
          {:else}
            <div class={listClass}>
              {#each cities as row (row.key)}
                {@const hasCoordinates = row.lat !== null && row.lon !== null}
                <div
                  role="presentation"
                  onmouseenter={() => hasCoordinates && controller?.focus([row.lat, row.lon])}
                  onmouseleave={() => hasCoordinates && controller?.resume()}
                  class={rowClass}
                >
                  {@render flag(row.country)}
                  <span class="flex min-w-0 flex-1 items-baseline gap-2" title={row.label}>
                    <span class="truncate text-[12px] text-black dark:text-fg">{row.name}</span>
                    {#if row.detail !== ''}
                      <span class="hidden shrink-0 text-[11px] text-neutral-400 md:inline dark:text-fg-faint">{row.detail}</span>
                    {/if}
                  </span>
                  {@render volume(row.requests, row.bytesOut, maxCityRequests)}
                </div>
              {:else}
                <Empty size="sm" title="No city data" description="No city data for the selected range." iconName="network" />
              {/each}
            </div>
          {/if}
        </div>
      </div>

      {#if attribution}
        <p class="px-4 py-2 text-[11px] text-neutral-400 dark:text-fg-faint">{attribution}</p>
      {/if}
    {/if}
  </div>
</SettingsSection>
