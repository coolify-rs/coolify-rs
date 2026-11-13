<script lang="ts">
  // Port of upstream components/forms/inherited-listbox.blade.php: a listbox whose first option
  // inherits the value of the level above. The inherit option has the value '' (stored as null).
  import Listbox from './Listbox.svelte';

  let {
    id,
    label,
    helper,
    options = [],
    inheritLabel = null,
    value = $bindable(''),
    disabled = false,
    onchange,
  }: {
    id: string;
    label?: string;
    helper?: string;
    options?: { value: string; label: string }[];
    /** "Use {level} default ({value})"; null at the top level, which has no inherit option. */
    inheritLabel?: string | null;
    value?: string;
    disabled?: boolean;
    /** Called with the chosen value, so the page saves it. */
    onchange?: (value: string) => void;
  } = $props();

  const all = $derived(inheritLabel === null ? options : [{ value: '', label: inheritLabel }, ...options]);
</script>

<Listbox {id} {label} {helper} {disabled} options={all} bind:value {onchange} />
