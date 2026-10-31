<script lang="ts">
  // Port of upstream components/notification/channel-actions.blade.php.
  import Reicon from './Reicon.svelte';

  let {
    enabled,
    canUpdate,
    ontoggle,
    ontest,
  }: { enabled: boolean; canUpdate: boolean; ontoggle: (next: boolean) => void; ontest: () => void } = $props();

  function valid(el: HTMLElement): boolean {
    return el.closest('form')?.reportValidity() ?? true;
  }
</script>

<div class="flex items-center gap-2">
  <button
    type="button"
    disabled={!canUpdate}
    class="button"
    class:button-highlighted={!enabled}
    onclick={(e) => {
      if (!enabled && !valid(e.currentTarget)) return;
      ontoggle(!enabled);
    }}
  >
    <span>{enabled ? 'Disable' : 'Enable'}</span>
  </button>
  <button
    type="button"
    disabled={!enabled || !canUpdate}
    class="button"
    onclick={(e) => {
      if (valid(e.currentTarget)) ontest();
    }}
  >
    <Reicon name="notifications" class="size-3.5" />
    Send test
  </button>
</div>
