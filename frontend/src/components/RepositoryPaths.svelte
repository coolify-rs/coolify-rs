<script lang="ts">
  // Port of upstream components/forms/repository-paths.blade.php: the base
  // directory and, optionally, a file path relative to it, with the resolved
  // path and the "Base directory is repeated" hint. Extra fields go in the slot.
  import type { Snippet } from 'svelte';
  import Callout from './Callout.svelte';
  import FormInput from './FormInput.svelte';

  let {
    base = $bindable('/'),
    file = $bindable<string | null>(null),
    fileLabel,
    defaultFile = '',
    showFile = false,
    disabled = false,
    class: cls = '',
    baseError,
    fileError,
    children,
  }: {
    base: string;
    file?: string | null;
    fileLabel?: string;
    defaultFile?: string;
    showFile?: boolean;
    disabled?: boolean;
    class?: string;
    baseError?: string;
    fileError?: string;
    children?: Snippet;
  } = $props();

  const uid = Math.random().toString(36).slice(2, 12);
  const normalize = (path: string | null | undefined) => {
    const p = (path ?? '').trim().replace(/\/+$/, '');
    return p === '' || p.startsWith('/') ? p : '/' + p;
  };
  const directory = $derived(normalize(base));
  const location = $derived(normalize(file) || defaultFile);
  const repeatsBase = $derived(directory !== '' && location.startsWith(directory + '/'));
  const suggestion = $derived(location.slice(directory.length));
  const fileValue = $derived(file ?? '');
</script>

<div class="grid gap-4 {cls}">
  <FormInput
    id="base-{uid}"
    label="Base directory"
    placeholder="/"
    helper="Repository directory used as the build root. Useful for monorepos."
    {disabled}
    bind:value={base}
    error={baseError}
    onblur={() => (base = directory || '/')}
  />
  {#if showFile}
    <FormInput
      id="file-{uid}"
      label={fileLabel}
      placeholder={defaultFile}
      helper="Path relative to the base directory."
      {disabled}
      bind:value={() => fileValue, (v) => (file = v)}
      error={fileError}
      onblur={() => (file = location)}
    />
    <p class="col-span-full text-xs text-neutral-500 dark:text-fg-dim">
      Resolved file:
      <code class="font-mono text-coollabs dark:text-brand-light">{directory + location}</code>
    </p>
    {#if repeatsBase}
      <Callout class="col-span-full" title="Base directory is repeated">
        {fileLabel} is relative to the base directory, so it should not start with
        <code class="rounded bg-black/10 px-1 dark:bg-white/10">{directory}</code> again.
        {#if !disabled}
          <button type="button" class="font-medium underline" onclick={() => (file = suggestion)}>Use <span>{suggestion}</span></button>
        {/if}
      </Callout>
    {/if}
  {/if}
  {@render children?.()}
</div>
