<script lang="ts">
  // Port of upstream livewire/analytics-placeholder.blade.php with components/skeleton/*.
  import SettingsSection from '../SettingsSection.svelte';

  let { scoped, hideSkeleton = false }: { scoped: boolean; hideSkeleton?: boolean } = $props();
  const skeleton = 'animate-pulse rounded-md bg-neutral-200/80 dark:bg-white/[0.06]';
  const widths = ['w-3/5', 'w-2/5', 'w-1/2', 'w-3/4', 'w-1/3', 'w-2/3'];
  const labels = ['Requests', 'Unique visitors', 'Bandwidth', 'Error rate', 'p95 latency'];
</script>

{#snippet table(rows: number)}
  <div class="flex flex-col">
    {#each Array(rows) as _, i (i)}
      <div class="flex min-h-11 items-center gap-3 border-b border-neutral-200 px-4 py-2 last:border-b-0 dark:border-white/[0.07]">
        <div aria-hidden="true" class="{skeleton} h-3 {widths[i % widths.length]} rounded-full"></div>
        <div aria-hidden="true" class="{skeleton} ml-auto h-3 w-10 shrink-0 rounded-full"></div>
      </div>
    {/each}
  </div>
{/snippet}

<div class="flex w-full min-w-0 flex-col gap-6">
  {#if !hideSkeleton}
    <div class="flex flex-col gap-4">
      {#if !scoped}
        <div class="min-w-0">
          <h1 class="min-w-0 text-[24px]! leading-7! font-semibold! tracking-tight!">Analytics</h1>
          <p class="mt-1 text-[13px] text-neutral-500 dark:text-fg-dim">Request traffic across every application and server, reported by Sentinel.</p>
        </div>
      {/if}
      <div class="flex flex-wrap items-center gap-2">
        {#if !scoped}
          <div aria-hidden="true" class="{skeleton} h-9 w-full rounded-lg sm:w-52"></div>
        {/if}
        <div aria-hidden="true" class="{skeleton} h-9 w-full rounded-lg sm:w-52"></div>
        <div aria-hidden="true" class="{skeleton} h-9 w-44 rounded-lg sm:ml-auto"></div>
      </div>
    </div>

    <SettingsSection id="analytics-overview-section" title="Overview">
      <div class="grid grid-cols-2 sm:grid-cols-3 lg:grid-cols-5 gap-px overflow-hidden rounded-lg bg-neutral-200 dark:bg-white/[0.07]">
        {#each labels as label (label)}
          <div class="flex flex-col bg-[var(--coollabs-base)] px-4 py-3">
            <span class="text-[11px] font-medium tracking-wide text-neutral-500 uppercase dark:text-fg-dim">{label}</span>
            <div aria-hidden="true" class="{skeleton} mt-2 h-6 w-20"></div>
            <div class="mt-auto pt-3">
              <div aria-hidden="true" class="{skeleton} h-9 w-full rounded"></div>
            </div>
          </div>
        {/each}
      </div>
    </SettingsSection>

    <SettingsSection id="analytics-requests-section" title="Requests">
      <div aria-hidden="true" class="{skeleton} h-[220px] w-full rounded-lg"></div>
    </SettingsSection>

    <SettingsSection id="analytics-status-codes-section" title="Status codes">
      <div aria-hidden="true" class="{skeleton} h-9 w-full rounded-lg"></div>
      <div class="mt-3 flex flex-wrap gap-2">
        <div aria-hidden="true" class="{skeleton} h-4 w-20 rounded-full"></div>
        <div aria-hidden="true" class="{skeleton} h-4 w-16 rounded-full"></div>
        <div aria-hidden="true" class="{skeleton} h-4 w-16 rounded-full"></div>
        <div aria-hidden="true" class="{skeleton} h-4 w-14 rounded-full"></div>
      </div>
    </SettingsSection>

    <div class="grid grid-cols-1 gap-6 lg:grid-cols-2">
      <SettingsSection id="analytics-hosts-section" title="Top hosts" flush>{@render table(6)}</SettingsSection>
      <SettingsSection id="analytics-apps-section" title="Top applications" flush>{@render table(6)}</SettingsSection>
    </div>

    <SettingsSection id="analytics-paths-section" title="Top paths" flush>{@render table(6)}</SettingsSection>

    <SettingsSection id="analytics-country-section" title="Locations" flush>
      {#snippet actions()}
        <div aria-hidden="true" class="{skeleton} h-9 w-36 rounded-lg"></div>
      {/snippet}
      <div class="grid grid-cols-1 items-start gap-4 p-4 lg:grid-cols-[2fr_3fr]">
        <div class="flex items-center justify-center">
          <div aria-hidden="true" class="{skeleton} aspect-square w-full max-w-[320px] rounded-full"></div>
        </div>
        {@render table(7)}
      </div>
    </SettingsSection>
  {/if}
</div>
