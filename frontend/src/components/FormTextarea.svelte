<script lang="ts">
  // Port of upstream components/forms/textarea.blade.php (plain variant).
  import Helper from './Helper.svelte';

  let {
    label,
    id,
    value = $bindable(''),
    error,
    helper,
    required = false,
    disabled = false,
    readonly = false,
    monospace = false,
    rows = 4,
    placeholder,
    oninput,
  }: {
    label?: string;
    id: string;
    value?: string;
    error?: string;
    helper?: string;
    required?: boolean;
    disabled?: boolean;
    readonly?: boolean;
    monospace?: boolean;
    rows?: number;
    placeholder?: string;
    oninput?: (e: Event) => void;
  } = $props();
</script>

<div class="flex-1 form-control">
  {#if label}
    <div class="mb-1.5 flex h-4 w-full items-center gap-1.5">
      <label for={id} class="mb-0! flex items-center gap-1 text-sm font-medium leading-4"
        >{label}
        {#if required}<span class="text-helper">*</span>{/if}
      </label>
      {#if helper}<Helper {helper} />{/if}
    </div>
  {/if}
  <textarea
    {placeholder}
    class="input scrollbar {monospace ? 'font-mono' : ''}"
    {disabled}
    {readonly}
    {required}
    {id}
    name={id}
    {rows}
    spellcheck="false"
    bind:value
    {oninput}
  ></textarea>
  {#if error}
    <label class="label" for={id}>
      <span class="text-red-500 label-text-alt">{error}</span>
    </label>
  {/if}
</div>
