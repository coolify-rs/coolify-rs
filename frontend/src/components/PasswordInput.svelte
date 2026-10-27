<script lang="ts">
  // Port of the password variant of upstream components/forms/input.blade.php
  // (eye toggle; glyphs are reicons "eye" and "eye-off2").
  import Reicon from './Reicon.svelte';

  let {
    value = $bindable(''),
    id,
    name,
    autocomplete,
    required = false,
    disabled = false,
    peek = true,
  }: {
    value?: string;
    id: string;
    name: string;
    autocomplete?: HTMLInputElement['autocomplete'];
    required?: boolean;
    disabled?: boolean;
    /** Upstream `allowToPeak`: show the eye toggle. */
    peek?: boolean;
  } = $props();

  let type = $state<'password' | 'text'>('password');
</script>

<div class="relative">
  <input
    {autocomplete}
    {type}
    class="input {peek ? 'input-with-password-toggle' : ''} {type === 'text' && !disabled ? 'truncate' : ''}"
    {required}
    {disabled}
    {id}
    {name}
    bind:value
  />
  {#if peek}<button
    type="button"
    onclick={() => (type = type === 'password' ? 'text' : 'password')}
    class="password-toggle flex absolute inset-y-0 right-0 z-10 items-center pr-2 cursor-pointer text-neutral-500 hover:text-black dark:text-neutral-400 dark:hover:text-white"
    aria-label="Toggle password visibility"
  >
    <Reicon name={type === 'password' ? 'eye' : 'eye-off2'} class="size-[18px]" />
  </button>{/if}
</div>
