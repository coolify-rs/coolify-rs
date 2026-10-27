<script lang="ts">
  // Port of upstream components/reicon.blade.php: glyphs come from the
  // generated src/upstream/reicons.ts (static upstream markup).
  import { reicons } from '../upstream/reicons';

  let { name, class: cls = '' }: { name: string; class?: string } = $props();
  const danger = $derived(name === 'danger');
  const markup = $derived(reicons[danger ? 'shield-alert' : name] ?? '');
</script>

<svg
  class={['size-4', danger && 'text-error', cls].filter(Boolean).join(' ')}
  viewBox="0 0 24 24"
  fill="none"
  xmlns="http://www.w3.org/2000/svg"
  aria-hidden="true"
>
  {@html markup}
</svg>
