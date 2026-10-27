<script lang="ts">
  // Port of upstream components/table/dropdown.blade.php: a listbox panel
  // fixed below (or above) its trigger, right-aligned to it.
  import type { Snippet } from 'svelte';

  let {
    panelClass = '',
    role = 'listbox',
    trigger,
    children,
  }: {
    panelClass?: string;
    role?: string;
    trigger: Snippet<[{ open: boolean }]>;
    children: Snippet<[{ close: () => void }]>;
  } = $props();

  let open = $state(false);
  let panelStyle = $state('position: fixed; min-width: 0; visibility: hidden;');
  let root: HTMLDivElement | undefined = $state();
  let triggerEl: HTMLDivElement | undefined = $state();
  let panel: HTMLDivElement | undefined = $state();

  function updatePosition() {
    if (!triggerEl || !panel) return;
    const t = triggerEl.getBoundingClientRect();
    const p = panel.getBoundingClientRect();
    const pad = 8;
    const left = Math.max(pad, Math.min(t.right - p.width, window.innerWidth - p.width - pad));
    const spaceBelow = window.innerHeight - t.bottom - pad;
    const top = spaceBelow >= p.height ? t.bottom + 4 : Math.max(pad, t.top - p.height - 4);
    panelStyle = `position: fixed; left: ${left}px; top: ${top}px; min-width: 0;`;
  }

  function toggle() {
    if (open) {
      open = false;
      return;
    }
    panelStyle = 'position: fixed; min-width: 0; visibility: hidden;';
    open = true;
    requestAnimationFrame(updatePosition);
  }

  const close = () => (open = false);
</script>

<svelte:window
  onclick={(e) => open && root && !root.contains(e.target as Node) && close()}
  onkeydown={(e) => e.key === 'Escape' && close()}
  onresize={() => open && updatePosition()}
  onscroll={() => open && updatePosition()}
/>

<div class="relative" bind:this={root}>
  <div bind:this={triggerEl} onclick={toggle} role="presentation">
    {@render trigger({ open })}
  </div>
  {#if open}
    <div
      bind:this={panel}
      style={panelStyle}
      class="listbox-panel origin-top fixed! right-auto! bottom-auto! z-[90]! mt-0! {panelClass}"
      {role}
    >
      {@render children({ close })}
    </div>
  {/if}
</div>
