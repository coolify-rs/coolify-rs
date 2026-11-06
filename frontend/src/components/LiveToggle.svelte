<script lang="ts">
  // Port of upstream components/live-toggle.blade.php. `storageKey` keeps the last choice the user
  // clicked; changes made by the page (a range that starts paused) are not stored. A stored "off"
  // always applies on load; a stored "on" applies only when `restoreLive` is true.
  let {
    live = $bindable(),
    storageKey,
    restoreLive = true,
    title = 'Turn live refresh on or off',
    onchange,
  }: { live: boolean; storageKey: string; restoreLive?: boolean; title?: string; onchange?: (live: boolean) => void } = $props();

  try {
    const stored = localStorage.getItem(storageKey);
    if (stored === '0') live = false;
    // svelte-ignore state_referenced_locally
    if (stored === '1' && restoreLive) live = true;
  } catch {
    // The component's default applies.
  }

  function toggle() {
    live = !live;
    try {
      localStorage.setItem(storageKey, live ? '1' : '0');
    } catch {
      // Kept for this page only.
    }
    onchange?.(live);
  }
</script>

<div>
  <button
    type="button"
    onclick={toggle}
    aria-pressed={live}
    {title}
    class="inline-flex h-7 items-center gap-1.5 rounded-md px-2.5 text-[12px] font-medium ring-1 ring-neutral-200 transition-colors dark:ring-white/[0.08] {live ? 'bg-white text-black shadow-sm dark:bg-white/[0.09] dark:text-fg' : 'text-neutral-500 hover:text-black dark:text-fg-faint dark:hover:text-fg'}"
  >
    {#if live}
      <span class="relative flex h-1.5 w-1.5 shrink-0" aria-hidden="true">
        <span class="absolute inline-flex h-full w-full rounded-full bg-emerald-500 opacity-75 motion-safe:animate-ping"></span>
        <span class="relative inline-flex h-1.5 w-1.5 rounded-full bg-emerald-500"></span>
      </span>
    {:else}
      <svg class="h-3 w-3 shrink-0" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
        <path d="M8 5.5v13a1 1 0 0 0 1.5.86l11-6.5a1 1 0 0 0 0-1.72l-11-6.5A1 1 0 0 0 8 5.5Z" />
      </svg>
    {/if}
    <span>{live ? 'Live' : 'Paused'}</span>
  </button>
</div>
