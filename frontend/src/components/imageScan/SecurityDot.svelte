<script lang="ts">
  // Port of upstream components/image-scan/security-dot.blade.php: a dot after a menu entry for
  // update_now (red) and update_available (yellow).
  let { status = null, class: cls = '' }: { status?: string | null; class?: string } = $props();

  const dot = $derived(
    status === 'update_now'
      ? { cls: 'bg-error', label: 'Security: update now' }
      : status === 'update_available'
        ? { cls: 'bg-warning', label: 'Security: security update' }
        : null,
  );
</script>

{#if dot}
  <span role="img" aria-label={dot.label} data-tooltip={dot.label} class="ml-auto size-2 shrink-0 rounded-full {dot.cls} {cls}"></span>
{/if}
