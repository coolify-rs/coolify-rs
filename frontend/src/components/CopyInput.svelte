<script lang="ts">
  // Port of upstream components/forms/copy-input.blade.php.
  import Reicon from './Reicon.svelte';

  let { label, text }: { label: string; text: string } = $props();
</script>

<div class="w-full">
  <!-- svelte-ignore a11y_label_has_associated_control -->
  <label class="flex gap-1 items-center mb-1 text-sm font-medium text-black dark:text-white">{label}</label>
  <div class="relative">
    <input
      type="text"
      value={text}
      class="input input-with-copy-button bg-white dark:bg-coolgray-100 dark:read-only:bg-coolgray-100 dark:read-only:text-white"
      readonly
      onfocus={(e) => e.currentTarget.select()}
    />
    <button
      type="button"
      title="Copy to clipboard"
      aria-label="Copy to clipboard"
      class="icon-button group shrink-0 absolute top-1/2 right-2 -translate-y-1/2"
      onclick={() => navigator.clipboard?.writeText(text).catch(() => {})}
    >
      <span class="inline-flex transition-transform duration-150 ease-out group-active:scale-75"><Reicon name="copy" class="size-3.5" /></span>
    </button>
  </div>
</div>
