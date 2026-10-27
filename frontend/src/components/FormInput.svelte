<script lang="ts">
  // Port of upstream components/forms/input.blade.php (+ View\Components\Forms\Input):
  // label row with optional helper; password fields with the peek toggle and,
  // unless `copyable={false}`, a copy button; number fields with −/+ steppers;
  // the validation message below.
  import type { HTMLInputAttributes } from 'svelte/elements';
  import Helper from './Helper.svelte';
  import Reicon from './Reicon.svelte';

  let {
    label,
    id,
    value = $bindable(''),
    error,
    helper,
    required = false,
    disabled = false,
    readonly = false,
    type = 'text',
    peek = true,
    copyable,
    ...rest
  }: {
    label?: string;
    id: string;
    value?: string;
    error?: string;
    helper?: string;
    required?: boolean;
    disabled?: boolean;
    readonly?: boolean;
    type?: string;
    /** Upstream `allowToPeak`. */
    peek?: boolean;
    /** Defaults to true for password fields. */
    copyable?: boolean;
  } & Omit<HTMLInputAttributes, 'value' | 'type'> = $props();

  let shown = $state(false);
  let copied = $state(false);
  let input: HTMLInputElement | undefined = $state();

  const isPassword = $derived(type === 'password');
  const hasPeek = $derived(isPassword && peek);
  const canCopy = $derived(copyable ?? isPassword);
  const classes = $derived(
    [
      'input',
      hasPeek && 'input-with-password-toggle',
      type === 'number' && 'input-with-stepper',
      canCopy && (hasPeek ? 'input-with-copy-and-peek' : 'input-with-copy-button'),
      isPassword && shown && !disabled && 'truncate',
    ]
      .filter(Boolean)
      .join(' '),
  );

  async function copy() {
    try {
      await navigator.clipboard.writeText(value ?? '');
      copied = true;
      setTimeout(() => (copied = false), 1500);
    } catch {
      // Clipboard access can be denied.
    }
  }

  /** number-stepper.js: step by the input's `step` within min/max. */
  function step(direction: number) {
    if (!input || input.disabled || input.readOnly) return;
    const stepBy = Number(input.step) || 1;
    let next = (Number(input.value) || 0) + direction * stepBy;
    if (input.min !== '') next = Math.max(Number(input.min), next);
    if (input.max !== '') next = Math.min(Number(input.max), next);
    value = String(next);
  }
</script>

{#snippet copyButton(cls: string)}
  <button
    type="button"
    title="Copy to clipboard"
    aria-label="Copy to clipboard"
    class="icon-button group shrink-0 {cls}"
    disabled={!value}
    onclick={copy}
  >
    <span class="inline-flex transition-transform duration-150 ease-out group-active:scale-75">
      {#if copied}<Reicon name="check" class="size-3.5 text-success" />{:else}<Reicon name="copy" class="size-3.5" />{/if}
    </span>
  </button>
{/snippet}

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
  {#if isPassword}
    <div class="relative">
      <input
        autocomplete="off"
        type={shown ? 'text' : 'password'}
        class={classes}
        {required}
        {disabled}
        {readonly}
        {id}
        name={id}
        bind:value
        {...rest}
      />
      {#if hasPeek}
        <button
          type="button"
          onclick={() => (shown = !shown)}
          class="password-toggle flex absolute inset-y-0 right-0 z-10 items-center pr-2 cursor-pointer text-neutral-500 hover:text-black dark:text-neutral-400 dark:hover:text-white"
          aria-label="Toggle password visibility"
        >
          <Reicon name={shown ? 'eye-off2' : 'eye'} class="size-[18px]" />
        </button>
      {/if}
      {#if canCopy}{@render copyButton(`absolute top-1/2 z-10 -translate-y-1/2 ${hasPeek ? 'right-8' : 'right-1'}`)}{/if}
    </div>
  {:else if type === 'number'}
    <div class="relative">
      <input bind:this={input} autocomplete="off" class={classes} {required} {readonly} type="number" {disabled} {id} name={id} bind:value {...rest} />
      <div class="number-stepper">
        <button type="button" tabindex="-1" aria-label="Decrease" onclick={() => step(-1)} disabled={disabled || readonly}>
          <Reicon name="minus" class="size-3.5" />
        </button>
        <button type="button" tabindex="-1" aria-label="Increase" onclick={() => step(1)} disabled={disabled || readonly}>
          <Reicon name="plus" class="size-3.5" />
        </button>
      </div>
    </div>
  {:else if canCopy}
    <div class="relative">
      <input autocomplete="off" class={classes} {required} {readonly} {type} {disabled} {id} name={id} bind:value {...rest} />
      {@render copyButton('absolute top-1/2 right-1 z-10 -translate-y-1/2')}
    </div>
  {:else}
    <input autocomplete="off" class={classes} {required} {readonly} {type} {disabled} {id} name={id} bind:value {...rest} />
  {/if}
  {#if error}
    <label class="label" for={id}>
      <span class="text-red-500 label-text-alt">{error}</span>
    </label>
  {/if}
</div>
