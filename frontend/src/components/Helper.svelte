<script lang="ts">
  // Port of upstream components/helper.blade.php: an info icon (or a custom
  // trigger) that shows a tooltip on hover, focus or tap.
  import type { Snippet } from 'svelte';
  import Reicon from './Reicon.svelte';

  let { helper, label = 'More information', trigger }: { helper: string; label?: string; trigger?: Snippet } = $props();

  let open = $state(false);
  let style = $state('');
  let hideTimer: ReturnType<typeof setTimeout> | undefined;
  let triggerEl: HTMLButtonElement | undefined = $state();
  let popup: HTMLDivElement | undefined = $state();

  function position() {
    if (!triggerEl || !popup) return;
    const pad = 8;
    const vw = window.innerWidth;
    const vh = window.innerHeight;
    style = `max-width: ${Math.min(320, vw - pad * 2)}px; max-height: ${vh - pad * 2}px; overflow-y: auto;`;
    const t = triggerEl.getBoundingClientRect();
    const p = popup.getBoundingClientRect();
    let top = t.bottom + pad;
    let left = t.right - p.width;
    if (top + p.height > vh - pad) top = t.top - p.height - pad;
    left = Math.min(Math.max(pad, left), vw - p.width - pad);
    top = Math.min(Math.max(pad, top), vh - p.height - pad);
    style += ` top: ${top}px; left: ${left}px;`;
  }
  function show() {
    clearTimeout(hideTimer);
    open = true;
    requestAnimationFrame(position);
  }
  function hide() {
    clearTimeout(hideTimer);
    hideTimer = setTimeout(() => (open = false), 150);
  }
</script>

<svelte:window
  onpointerdown={(e) => open && !triggerEl?.contains(e.target as Node) && !popup?.contains(e.target as Node) && (open = false)}
  onkeydown={(e) => e.key === 'Escape' && (open = false)}
/>

<div class="relative inline-flex align-middle">
  <button
    type="button"
    bind:this={triggerEl}
    data-icon-tooltip-ignore
    class="info-helper relative inline-flex shrink-0 items-center justify-center border-0 bg-transparent p-0 leading-none {trigger ? '' : 'size-3.5'}"
    aria-label={label}
    onmouseenter={show}
    onmouseleave={hide}
    onfocus={show}
    onblur={hide}
    onclick={(e) => {
      e.preventDefault();
      e.stopPropagation();
      show();
    }}
  >
    {#if trigger}
      {@render trigger()}
    {:else}
      <Reicon
        name="info-circle"
        class="size-3.5 text-neutral-400 transition-colors hover:text-neutral-600 dark:text-fg-faint dark:hover:text-fg-dim"
      />
    {/if}
  </button>
  {#if open}
    <div
      bind:this={popup}
      role="tooltip"
      {style}
      class="info-helper-popup fixed z-[10000] w-max max-w-[min(20rem,calc(100vw-2rem))] whitespace-normal"
      onmouseenter={() => clearTimeout(hideTimer)}
      onmouseleave={hide}
    >
      <div class="px-3 py-2.5 text-[13px] leading-5">{helper}</div>
    </div>
  {/if}
</div>
