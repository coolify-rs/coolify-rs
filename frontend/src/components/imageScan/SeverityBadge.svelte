<script lang="ts">
  // Port of upstream components/image-scan/severity-badge.blade.php.
  import StatusBadge from '../StatusBadge.svelte';

  let { severity = 'unknown', count = null, class: cls = '' }: { severity?: string; count?: number | string | null; class?: string } = $props();

  const level = $derived((['critical', 'high', 'medium', 'low'] as const).find((s) => s === severity) ?? 'unknown');
  const type = $derived(level === 'unknown' ? 'neutral' : (`severity-${level}` as const));
  const label = $derived(count === null ? level.charAt(0).toUpperCase() + level.slice(1) : `${count} ${level}`);
</script>

<StatusBadge {type} {label} class={cls} />
