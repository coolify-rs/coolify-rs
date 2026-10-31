<script lang="ts">
  // Port of upstream livewire/traffic/_device-chart.blade.php.
  import Empty from '../Empty.svelte';
  import DeviceDonut from './DeviceDonut.svelte';

  let { chartId, labels, series }: { chartId: string; labels: string[]; series: number[] } = $props();

  const hasData = $derived(series.reduce((sum, v) => sum + v, 0) > 0);
</script>

{#if !hasData}
  <Empty size="sm" title="No device data" description="No device data for the selected range." iconName="network" />
{:else}
  <DeviceDonut {chartId} {labels} {series} />
{/if}
