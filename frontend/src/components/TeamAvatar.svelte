<script lang="ts">
  // Port of upstream components/team/avatar.blade.php.
  let { name, url, class: cls = '' }: { name: string; url?: string | null; class?: string } = $props();
</script>

{#if url}
  <img src={url} alt={name} class="shrink-0 rounded-md object-cover {cls}" />
{:else}
  <span class="flex shrink-0 items-center justify-center rounded-md bg-neutral-200 font-semibold text-neutral-700 dark:bg-white/[0.1] dark:text-fg {cls}">
    {name.slice(0, 1).toUpperCase()}
  </span>
{/if}
