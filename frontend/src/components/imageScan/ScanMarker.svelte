<script lang="ts" module>
  /** `VulnerabilityMarkers::marker()`. */
  export interface Marker {
    status: string;
    text: string;
    label: string;
    tooltip: string | null;
  }
</script>

<script lang="ts">
  // Port of upstream components/image-scan/marker.blade.php: the security marker of lists and
  // canvas cards: update_now (error icon) or no_fix (neutral icon). Other statuses render nothing.
  import Reicon from '../Reicon.svelte';

  let { marker = null, class: cls = '' }: { marker?: Marker | null; class?: string } = $props();

  const tones: Record<string, string> = { update_now: 'text-error', no_fix: 'text-neutral-500 dark:text-fg-dim' };
</script>

{#if marker && tones[marker.status] && marker.text}
  <span
    role="img"
    aria-label={marker.label}
    data-tooltip={marker.tooltip || undefined}
    data-tooltip-wrap={marker.tooltip ? '' : undefined}
    class="inline-flex shrink-0 items-center gap-1 text-xs font-medium leading-none text-neutral-700 dark:text-white {marker.tooltip ? 'cursor-help' : ''} {cls}"
  >
    <Reicon name="shield-alert" class="size-4 shrink-0 {tones[marker.status]}" />
    <span aria-hidden="true">{marker.text}</span>
  </span>
{/if}
