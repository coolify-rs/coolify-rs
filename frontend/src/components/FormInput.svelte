<script lang="ts">
  // Port of upstream components/forms/input.blade.php (text and password
  // variants): label row with optional helper, input, validation message.
  import type { HTMLInputAttributes } from 'svelte/elements';
  import PasswordInput from './PasswordInput.svelte';
  import Helper from './Helper.svelte';

  let {
    label,
    id,
    value = $bindable(''),
    error,
    helper,
    required = false,
    disabled = false,
    type = 'text',
    peek = true,
    ...rest
  }: {
    label?: string;
    id: string;
    value?: string;
    error?: string;
    helper?: string;
    required?: boolean;
    disabled?: boolean;
    type?: string;
    peek?: boolean;
  } & Omit<HTMLInputAttributes, 'value' | 'type'> = $props();
</script>

<div class="w-full">
  {#if label}
    <div class="mb-1.5 flex h-4 w-full items-center gap-1.5">
      <label for={id} class="mb-0! flex items-center gap-1 text-sm font-medium leading-4"
        >{label}
        {#if required}<span class="text-helper">*</span>{/if}
      </label>
      {#if helper}<Helper {helper} />{/if}
    </div>
  {/if}
  {#if type === 'password'}
    <PasswordInput {id} name={id} {required} {disabled} {peek} bind:value />
  {:else}
    <input autocomplete="off" class="input" {required} {disabled} {type} {id} name={id} bind:value {...rest} />
  {/if}
  {#if error}
    <label class="label" for={id}>
      <span class="text-red-500 label-text-alt">{error}</span>
    </label>
  {/if}
</div>
