<script lang="ts">
  // Port of upstream components/unsaved-bar.blade.php: a floating pill that
  // shows while the form has unsaved changes. Enter saves, Reset reverts.
  let {
    dirty,
    saving = false,
    onsave,
    onreset,
    label = "You have changes that haven't been saved yet.",
  }: { dirty: boolean; saving?: boolean; onsave: () => void; onreset: () => void; label?: string } = $props();

  function onkeydown(e: KeyboardEvent) {
    if (e.key !== 'Enter' || !dirty || saving || e.repeat || e.isComposing) return;
    if (e.shiftKey || e.ctrlKey || e.altKey || e.metaKey) return;
    if ((e.target as HTMLElement).closest('textarea, button, a, select, [contenteditable=true]')) return;
    e.preventDefault();
    onsave();
  }
</script>

<svelte:window {onkeydown} />

<div
  class="pointer-events-none fixed inset-x-3 bottom-[calc(var(--keyboard-inset,0px)+max(1.5rem,env(safe-area-inset-bottom,0px)+0.75rem))] z-[1000] flex max-w-full translate-y-6 scale-95 flex-col items-stretch gap-2 rounded-2xl border border-neutral-200 bg-white py-2.5 pr-2.5 pl-4 opacity-0 shadow-modal transition-[opacity,transform,scale] duration-300 ease-[cubic-bezier(0.16,1,0.3,1)] delay-0 dark:border-white/10 dark:bg-surface [&.is-dirty]:pointer-events-auto [&.is-dirty]:translate-y-0 [&.is-dirty]:scale-100 [&.is-dirty]:opacity-100 [&.is-dirty]:delay-300 [&.is-saving]:pointer-events-none [&.is-saving]:translate-y-6 [&.is-saving]:scale-95 [&.is-saving]:opacity-0 [&.is-saving]:duration-200 [&.is-saving]:ease-in [&.is-saving]:delay-0 sm:inset-x-auto sm:left-1/2 sm:bottom-6 sm:w-max sm:max-w-none sm:-translate-x-1/2 sm:flex-row sm:items-center sm:gap-8 sm:py-2 sm:pl-5 sm:pr-2"
  class:is-dirty={dirty}
  class:is-saving={saving}
>
  <span class="text-[13px] font-semibold leading-snug text-neutral-800 dark:text-fg sm:whitespace-nowrap">{label}</span>
  <div class="flex shrink-0 items-center justify-end gap-2">
    <button
      type="button"
      onclick={onreset}
      class="h-8 rounded-lg bg-neutral-100 px-3.5 text-[13px] font-medium text-neutral-700 transition-colors hover:bg-neutral-200 dark:bg-white/[0.07] dark:text-fg dark:hover:bg-white/[0.12]"
    >
      Reset
    </button>
    <button
      type="button"
      onclick={onsave}
      disabled={saving}
      class="button-highlighted flex h-8 items-center gap-2 rounded-lg px-4 text-[13px] font-semibold transition-[transform,background-color] active:scale-[0.98]"
    >
      <span>Save changes</span>
      <kbd class="rounded border border-current/20 bg-current/10 px-1.5 py-0.5 text-[10px] leading-none font-medium text-current">Enter</kbd>
    </button>
  </div>
</div>
