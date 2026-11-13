<script lang="ts">
  // Port of the custom theme dialog of upstream components/theme-controls.blade.php:
  // presets, a preview, four colors and a share code. Save applies the draft.
  import Reicon from './Reicon.svelte';
  import ThemeSwatch from './ThemeSwatch.svelte';
  import { portal } from '../lib/portal';
  import {
    customThemePresets as presets,
    parseCustomTheme,
    readableTextOn as textOn,
    serializeCustomTheme,
    themeAccentFill as accentFill,
    themeAccentForeground as accentTextOn,
    type CustomTheme,
    type ThemePreset,
  } from '../lib/customTheme';

  let {
    initial,
    onclose,
    onsave,
  }: {
    /** A null draft is the built-in Dark theme. */
    initial: CustomTheme | null;
    onclose: () => void;
    onsave: (draft: CustomTheme | null) => void;
  } = $props();

  const startingDraft = () => initial;
  let draft = $state<CustomTheme | null>(startingDraft());
  const draftColors = $derived(draft ?? presets[0].swatch!);
  let shareCode = $state(serializeCustomTheme(startingDraft() ?? presets[0].swatch!));
  let shareCodeInvalid = $state(false);
  let shareCodeCopied = $state(false);
  let shareInput = $state<HTMLInputElement>();

  function setDraft(theme: CustomTheme | null) {
    draft = theme;
    shareCode = serializeCustomTheme(theme ?? presets[0].swatch!);
    shareCodeInvalid = false;
  }
  function setDraftColor(key: keyof CustomTheme, value: string) {
    const theme = parseCustomTheme(serializeCustomTheme({ ...draftColors, [key]: value }));
    if (theme) setDraft(theme);
  }
  function readShareCode(code: string) {
    const theme = parseCustomTheme(code);
    if (theme) {
      setDraft(theme);
      return;
    }
    shareCodeInvalid = code.trim() !== '';
  }
  async function copyShareCode() {
    try {
      await navigator.clipboard.writeText(shareCode);
      shareCodeCopied = true;
      setTimeout(() => (shareCodeCopied = false), 1500);
    } catch {
      shareInput?.select();
    }
  }
  function isDraftPreset(preset: ThemePreset) {
    return preset.theme === null || draft === null ? preset.theme === draft : serializeCustomTheme(preset.theme) === serializeCustomTheme(draft);
  }

  // `x-trap.inert.noscroll`. Alpine's `.inert` only sets aria-hidden on the rest of the page, so
  // what sits above the dialog (the layout popups) stays clickable, and Tab stays in the dialog.
  function trap(node: HTMLElement) {
    const previous = document.activeElement as HTMLElement | null;
    const others = [...document.body.children].filter((e) => e !== node) as HTMLElement[];
    const hidden = others.map((e) => e.getAttribute('aria-hidden'));
    others.forEach((e) => e.setAttribute('aria-hidden', 'true'));
    const overflow = document.documentElement.style.overflow;
    document.documentElement.style.overflow = 'hidden';
    const tabbable = () => [...node.querySelectorAll<HTMLElement>('button, input')];
    tabbable()[0]?.focus();
    function onkeydown(e: KeyboardEvent) {
      if (e.key !== 'Tab') return;
      const all = tabbable();
      const edge = e.shiftKey ? all[0] : all[all.length - 1];
      if (document.activeElement === edge) {
        e.preventDefault();
        (e.shiftKey ? all[all.length - 1] : all[0])?.focus();
      }
    }
    node.addEventListener('keydown', onkeydown);
    return {
      destroy() {
        node.removeEventListener('keydown', onkeydown);
        others.forEach((e, i) => (hidden[i] === null ? e.removeAttribute('aria-hidden') : e.setAttribute('aria-hidden', hidden[i]!)));
        document.documentElement.style.overflow = overflow;
        previous?.focus?.();
      },
    };
  }

  const colorFields: { key: keyof CustomTheme; label: string; description: string }[] = [
    { key: 'sidebar', label: 'Sidebar', description: 'The navigation on the left' },
    { key: 'selected', label: 'Selected', description: 'The current page in the sidebar' },
    { key: 'accent', label: 'Accent', description: 'Buttons, links, and highlights' },
    { key: 'background', label: 'Background', description: 'Pages, cards, and panels' },
  ];
</script>

<svelte:window onkeydown={(e) => e.key === 'Escape' && onclose()} />

<div class="fixed inset-0 z-99 overflow-hidden" use:portal use:trap>
  <div class="absolute inset-0 h-full w-full bg-black/50 backdrop-blur-[2px]"></div>
  <div
    role="presentation"
    onclick={(e) => e.target === e.currentTarget && onclose()}
    class="relative flex min-h-full items-start justify-center p-2 sm:items-center sm:p-4"
  >
    <div
      role="dialog"
      aria-modal="true"
      aria-labelledby="custom-theme-title"
      class="application-settings-form application-settings-section relative flex max-h-[calc(100dvh-1rem)] w-full max-w-5xl flex-col overflow-hidden sm:max-h-[calc(100dvh-2rem)]"
      style="box-shadow: 0 0 0 1px var(--coollabs-hairline), var(--shadow-modal)"
    >
      <header class="shrink-0">
        <div class="min-w-0 flex-1 py-0.5">
          <h3 id="custom-theme-title" class="truncate">Custom theme</h3>
          <p class="mt-0.5 text-xs text-neutral-500 dark:text-fg-dim">Colors of coolify-rs in this browser. Text stays readable on every color.</p>
        </div>
        <button
          type="button"
          onclick={onclose}
          class="flex size-7 shrink-0 cursor-pointer items-center justify-center rounded-md text-neutral-500 outline-0 transition-colors hover:bg-neutral-100 hover:text-black focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-accent dark:text-fg-faint dark:hover:bg-white/[0.06] dark:hover:text-fg"
        >
          <Reicon name="x" class="size-4" />
          <span class="sr-only">Close</span>
        </button>
      </header>
      <div class="application-settings-section-body grid min-h-0 flex-1 gap-6 overflow-y-auto lg:grid-cols-[minmax(0,1fr)_minmax(0,1.1fr)]">
        <div>
          <div class="mb-2 text-xs font-medium text-neutral-500 dark:text-fg-dim">Presets</div>
          <div class="grid grid-cols-2 gap-3 sm:grid-cols-3">
            {#each presets as preset (preset.name)}
              <button
                type="button"
                onclick={() => setDraft(preset.theme && { ...preset.theme })}
                aria-pressed={isDraftPreset(preset)}
                class="group flex flex-col gap-1.5 rounded-lg p-1 text-left outline-0 focus-visible:ring-1 focus-visible:ring-accent"
              >
                <ThemeSwatch
                  theme={preset.theme ?? preset.swatch!}
                  class="h-14 w-full transition-shadow group-hover:shadow-md {isDraftPreset(preset)
                    ? 'ring-2 ring-coollabs ring-offset-2 ring-offset-white dark:ring-brand-light dark:ring-offset-panel'
                    : ''}"
                />
                <span class="truncate text-xs text-neutral-600 dark:text-fg-dim">{preset.name}</span>
              </button>
            {/each}
          </div>
        </div>

        <div class="flex min-w-0 flex-col gap-5">
          <!-- Preview of a page with the draft colors. -->
          <div class="flex h-48 overflow-hidden rounded-lg border border-neutral-200 text-xs dark:border-white/[0.08]" aria-hidden="true">
            <div class="flex w-2/5 flex-col gap-0.5 p-2.5" style="background: {draftColors.sidebar}; color: {textOn(draftColors.sidebar)}">
              <div class="mb-2 px-2 text-sm font-semibold">coolify-rs</div>
              <div class="flex items-center gap-2 rounded-md px-2 py-1.5 opacity-75">
                <Reicon name="dashboard" class="size-3.5" /> Dashboard
              </div>
              <div class="flex items-center gap-2 rounded-md px-2 py-1.5 font-medium" style="background: {draftColors.selected}; color: {textOn(draftColors.selected)}">
                <Reicon name="projects" class="size-3.5" /> Projects
              </div>
              <div class="flex items-center gap-2 rounded-md px-2 py-1.5 opacity-75">
                <Reicon name="servers" class="size-3.5" /> Servers
                <span class="ml-auto rounded-full px-1.5 text-[10px] font-semibold" style="background: {accentFill(draftColors.accent)}; color: {accentTextOn(draftColors.accent)}">3</span>
              </div>
            </div>
            <div class="flex min-w-0 flex-1 flex-col gap-2 p-3" style="background: {draftColors.background}; color: {textOn(draftColors.background)}">
              <div class="flex items-center justify-between gap-2">
                <span class="text-sm font-semibold">Projects</span>
                <span class="rounded-md px-2 py-1 text-[11px] font-medium" style="background: {accentFill(draftColors.accent)}; color: {accentTextOn(draftColors.accent)}">+ New</span>
              </div>
              {#each [['My first project', 'production'], ['Website', 'staging']] as [name, env] (name)}
                <div
                  class="rounded-md border p-2.5"
                  style="background: color-mix(in oklab, {textOn(draftColors.background)} 4%, {draftColors.background}); border-color: color-mix(in oklab, {textOn(draftColors.background)} 14%, {draftColors.background})"
                >
                  <div class="font-medium">{name}</div>
                  <div class="mt-0.5 opacity-60">{env}</div>
                </div>
              {/each}
            </div>
          </div>

          <div>
            <div class="mb-2 text-xs font-medium text-neutral-500 dark:text-fg-dim">Colors</div>
            <div class="flex flex-col gap-2.5">
              {#each colorFields as field (field.key)}
                <div class="flex items-center gap-3">
                  <label class="relative size-9 shrink-0 cursor-pointer overflow-hidden rounded-md border border-black/10 shadow-sm dark:border-white/15">
                    <span class="absolute inset-0" style="background: {draftColors[field.key]}"></span>
                    <input
                      type="color"
                      autocomplete="off"
                      value={draftColors[field.key]}
                      oninput={(e) => setDraftColor(field.key, e.currentTarget.value)}
                      aria-label="{field.label} color"
                      class="absolute inset-0 h-full w-full cursor-pointer opacity-0"
                    />
                  </label>
                  <div class="min-w-0 flex-1">
                    <div class="text-sm font-medium text-black dark:text-fg">{field.label}</div>
                    <div class="truncate text-xs text-neutral-500 dark:text-fg-dim">{field.description}</div>
                  </div>
                  <input
                    type="text"
                    autocomplete="off"
                    spellcheck="false"
                    maxlength="7"
                    value={draftColors[field.key]}
                    onchange={(e) => {
                      setDraftColor(field.key, e.currentTarget.value);
                      e.currentTarget.value = draftColors[field.key];
                    }}
                    aria-label="{field.label} hex color"
                    class="input w-28! shrink-0 font-mono text-sm"
                  />
                </div>
              {/each}
            </div>
          </div>

          <div>
            <label for="custom-theme-share-code" class="mb-2 block text-xs font-medium text-neutral-500 dark:text-fg-dim">Share code</label>
            <div class="flex gap-2">
              <input
                id="custom-theme-share-code"
                bind:this={shareInput}
                type="text"
                autocomplete="off"
                spellcheck="false"
                value={shareCode}
                oninput={(e) => readShareCode(e.currentTarget.value)}
                onblur={(e) => {
                  e.currentTarget.value = shareCode;
                  shareCodeInvalid = false;
                }}
                aria-invalid={shareCodeInvalid}
                class="input min-w-0 flex-1 font-mono text-sm {shareCodeInvalid ? 'border-error! ring-error!' : ''}"
              />
              <button type="button" onclick={copyShareCode} aria-label="Copy share code" class="button shrink-0 px-2.5">
                {#if shareCodeCopied}
                  <Reicon name="check" class="size-4 text-success" />
                {:else}
                  <Reicon name="copy" class="size-4" />
                {/if}
              </button>
            </div>
            <p class="mt-1.5 text-xs {shareCodeInvalid ? 'text-error' : 'text-neutral-500 dark:text-fg-dim'}">
              {shareCodeInvalid ? 'Enter four hex colors, separated by commas.' : 'Paste a code that someone shared, or copy yours.'}
            </p>
          </div>
        </div>
      </div>
      <footer class="flex shrink-0 flex-wrap items-center justify-end gap-2 border-t border-neutral-200 px-4 py-3 dark:border-white/[0.08]">
        <button type="button" class="button" onclick={onclose}>Cancel</button>
        <button type="button" class="button" {...{ ishighlighted: '' }} onclick={() => onsave(draft)}>Save theme</button>
      </footer>
    </div>
  </div>
</div>
