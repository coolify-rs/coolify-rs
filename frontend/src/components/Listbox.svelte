<script lang="ts" generics="V extends string | boolean">
  // Port of upstream components/forms/listbox.blade.php (single select): a
  // trigger button and a fixed-position option panel.
  import Helper from './Helper.svelte';

  interface Option {
    value: V;
    label: string;
    description?: string;
    disabled?: boolean;
  }

  let {
    id,
    label,
    helper,
    required = false,
    options,
    value = $bindable(),
    disabled = false,
    placeholder = 'Select…',
    emptyText = 'No options available.',
    onchange,
  }: {
    id: string;
    label?: string;
    helper?: string;
    required?: boolean;
    options: Option[];
    value: V;
    disabled?: boolean;
    placeholder?: string;
    emptyText?: string;
    onchange?: (v: V) => void;
  } = $props();

  let open = $state(false);
  let positioned = $state(false);
  let panelStyle = $state('');
  let root: HTMLDivElement | undefined = $state();
  let trigger: HTMLButtonElement | undefined = $state();
  let panel: HTMLDivElement | undefined = $state();

  const current = $derived(options.find((o) => String(o.value) === String(value))?.label ?? placeholder);

  function position() {
    if (!trigger || !panel) return;
    const gap = 4;
    const edge = 12;
    const t = trigger.getBoundingClientRect();
    panel.style.width = 'max-content';
    panel.style.minWidth = `${t.width}px`;
    panel.style.maxWidth = `${window.innerWidth - edge * 2}px`;
    const width = Math.min(Math.max(t.width, panel.offsetWidth), window.innerWidth - edge * 2);
    const height = Math.min(panel.scrollHeight, 256);
    const fitsBelow = window.innerHeight - t.bottom - gap >= height;
    const top = fitsBelow ? t.bottom + gap : Math.max(edge, t.top - gap - height);
    const left = Math.min(Math.max(edge, t.left), window.innerWidth - width - edge);
    panelStyle = `top: ${top}px; left: ${left}px; width: ${width}px; min-width: ${t.width}px; max-width: ${window.innerWidth - edge * 2}px;`;
    positioned = true;
  }

  function toggle() {
    open = !open;
    positioned = false;
    if (open) requestAnimationFrame(position);
  }

  function choose(o: Option) {
    if (o.disabled) return;
    open = false;
    if (String(o.value) === String(value)) return;
    value = o.value;
    onchange?.(o.value);
  }
</script>

<svelte:window
  onclick={(e) => open && root && !root.contains(e.target as Node) && !panel?.contains(e.target as Node) && (open = false)}
  onresize={() => open && position()}
/>

<div class="w-full min-w-0">
  {#if label}
    <div class="mb-1.5 flex h-4 w-full items-center gap-1.5">
      <label for="{id}-trigger" class="mb-0! flex items-center gap-1.5 leading-4">
        {label}
        {#if required}<span class="text-helper">*</span>{/if}
      </label>
      {#if helper}<Helper {helper} />{/if}
    </div>
  {/if}
  <div class="relative min-w-0" bind:this={root} onkeydown={(e) => e.key === 'Escape' && (open = false)} role="presentation">
    <button
      bind:this={trigger}
      id="{id}-trigger"
      type="button"
      class="listbox-trigger"
      onclick={toggle}
      {disabled}
      aria-haspopup="listbox"
      aria-expanded={open}
      title={current}
    >
      <span class="listbox-trigger-label flex items-center gap-2">
        <span class="min-w-0 truncate">{current}</span>
      </span>
      <svg xmlns="http://www.w3.org/2000/svg" fill="none" viewBox="0 0 24 24" stroke-width="2" stroke="currentColor" class="size-3.5 shrink-0 opacity-60">
        <path stroke-linecap="round" stroke-linejoin="round" d="m8 9 4-4 4 4m0 6-4 4-4-4" />
      </svg>
    </button>
    {#if open}
      <div
        bind:this={panel}
        id="{id}-panel"
        class="listbox-panel"
        style="position: fixed; z-index: 9999; visibility: {positioned ? 'visible' : 'hidden'}; {panelStyle}"
        role="listbox"
      >
        {#if options.length === 0}
          <div class="px-3 py-2 text-[13px] text-neutral-500 dark:text-fg-dim">{emptyText}</div>
        {/if}
        {#each options as o (String(o.value))}
          <div>
            <button
              type="button"
              class="listbox-option {o.disabled ? 'listbox-option-disabled' : ''}"
              role="option"
              aria-selected={String(o.value) === String(value)}
              onclick={() => choose(o)}
            >
              <span class="flex min-w-0 items-center gap-2 text-left">
                <span class="flex min-w-0 flex-col items-start">
                  <span class="flex min-w-0 items-baseline gap-2">
                    <span class="truncate">{o.label}</span>
                  </span>
                  {#if o.description}
                    <span class="text-xs whitespace-normal text-neutral-500 dark:text-fg-dim">{o.description}</span>
                  {/if}
                </span>
              </span>
              {#if String(o.value) === String(value)}
                <svg xmlns="http://www.w3.org/2000/svg" fill="none" viewBox="0 0 24 24" stroke-width="2.5" stroke="currentColor" class="ml-auto size-3.5 shrink-0">
                  <path stroke-linecap="round" stroke-linejoin="round" d="m4.5 12.75 6 6 9-13.5" />
                </svg>
              {/if}
            </button>
          </div>
        {/each}
      </div>
    {/if}
  </div>
</div>
