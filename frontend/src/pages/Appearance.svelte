<script lang="ts">
  // Port of upstream livewire/profile/appearance.blade.php
  // (components/theme-controls.blade.php) without the custom color theme
  // and chart time settings, which coolify-rs has no use for yet.
  import Reicon from '../components/Reicon.svelte';
  import { layout, type Theme } from '../lib/layout.svelte';

  const card =
    'group relative overflow-hidden rounded-[10px] border border-neutral-200 bg-white text-left transition-[border-color,box-shadow] hover:border-neutral-300 hover:shadow-sm dark:border-white/[0.07] dark:bg-white/[0.05] dark:hover:border-white/[0.12]';
  const selected = 'border-coollabs! bg-coollabs/[0.06]! ring-2 ring-coollabs/25 dark:border-brand-light! dark:bg-brand-light/[0.08]! dark:ring-brand-light/25';

  const themes: { value: Theme; label: string; description: string; preview: string }[] = [
    { value: 'light', label: 'Light', description: 'Bright surfaces and dark text.', preview: 'bg-white' },
    { value: 'system', label: 'System', description: 'Follow your operating system.', preview: 'bg-gradient-to-r from-white via-neutral-400 to-[#050505]' },
    { value: 'dark', label: 'Dark', description: 'Dark surfaces and soft contrast.', preview: 'bg-[#181818]' },
  ];
  const widths = [
    { value: 'full', label: 'Full width', description: 'Use all available space for page content.' },
    { value: 'centered', label: 'Centered', description: 'Keep content centered at a comfortable maximum width.' },
  ];
  const navStyles = [
    { value: 'default', label: 'Default', description: 'Top bar and sidebars run edge to edge.' },
    { value: 'floating', label: 'Floating', description: 'Framed panels float on a dotted backdrop, like the canvas.' },
  ];
  const navItems = [
    { value: '3d', label: '3D', description: 'Buttons and sidebar icons sit on a raised edge that lifts on hover.' },
    { value: 'flat', label: 'Flat', description: 'No raised edges. Sidebar items are plain rows with a highlight for the current page.' },
  ];
</script>

{#snippet footer(label: string, description: string, active: boolean)}
  <div class="p-3">
    <div class="flex items-center justify-between gap-2">
      <span class="text-sm font-semibold text-black dark:text-fg">{label}</span>
      {#if active}<Reicon name="check-circle" class="size-4 text-coollabs dark:text-brand-light" />{/if}
    </div>
    <p class="mt-1 text-xs leading-5 text-neutral-500 dark:text-fg-dim">{description}</p>
  </div>
{/snippet}

<div>
  <div class="mt-8 flex w-full max-w-none flex-col gap-6 lg:mt-3">
    <section class="application-settings-section">
      <div class="application-settings-section-header">
        <div>
          <h2>Color theme</h2>
          <p>Choose the color scheme used in this browser.</p>
        </div>
      </div>
      <div class="application-settings-section-body grid gap-3 sm:grid-cols-2 lg:grid-cols-4">
        {#each themes as o (o.value)}
          <div
            role="button"
            tabindex="0"
            onclick={() => layout.setTheme(o.value)}
            onkeydown={(e) => e.key === 'Enter' && (e.preventDefault(), layout.setTheme(o.value))}
            class="{card} {layout.theme === o.value ? selected : ''}"
          >
            <div class="h-20 {o.preview} border-b border-neutral-200 dark:border-white/[0.07]">
              <div class="flex h-full items-center justify-center">
                <div class="h-8 w-20 rounded-md border border-black/10 bg-neutral-100/80 shadow-sm dark:border-white/10 dark:bg-black/20"></div>
              </div>
            </div>
            {@render footer(o.label, o.description, layout.theme === o.value)}
          </div>
        {/each}
      </div>
    </section>

    <section class="application-settings-section">
      <div class="application-settings-section-header">
        <div>
          <h2>Page width</h2>
          <p>Choose how content uses the available browser width.</p>
        </div>
      </div>
      <div class="application-settings-section-body grid gap-3 sm:grid-cols-2">
        {#each widths as o (o.value)}
          <button type="button" onclick={() => layout.setPageWidth(o.value)} class="{card} {layout.pageWidth === o.value ? selected : ''}">
            <div class="flex h-20 items-center border-b border-neutral-200 bg-neutral-50 px-4 dark:border-white/[0.07] dark:bg-black/15">
              <div class="flex h-11 w-full gap-1.5 rounded-md border border-neutral-300 bg-white p-1.5 dark:border-white/15 dark:bg-[#181818]">
                <div class="w-3 shrink-0 rounded-sm bg-neutral-200 dark:bg-white/10"></div>
                <div class="h-full rounded-sm bg-neutral-200 dark:bg-white/10 {o.value === 'full' ? 'w-full' : 'mx-auto w-2/3'}"></div>
              </div>
            </div>
            {@render footer(o.label, o.description, layout.pageWidth === o.value)}
          </button>
        {/each}
      </div>
    </section>

    <section class="application-settings-section">
      <div class="application-settings-section-header">
        <div>
          <h2>Navigation style</h2>
          <p>Choose how the top bar and sidebars sit on the page.</p>
        </div>
      </div>
      <div class="application-settings-section-body grid gap-3 sm:grid-cols-2">
        {#each navStyles as o (o.value)}
          {@const floating = o.value === 'floating'}
          <button type="button" onclick={() => layout.setNavStyle(o.value)} class="{card} {layout.navStyle === o.value ? selected : ''}">
            <div class="flex h-20 border-b border-neutral-200 bg-neutral-50 dark:border-white/[0.07] dark:bg-black/15 {floating ? 'gap-1.5 p-1.5' : ''}">
              <div
                class="flex flex-col items-center gap-1.5 bg-white px-2 py-2 dark:bg-white/[0.04] {floating
                  ? 'rounded-lg border border-neutral-200 dark:border-white/[0.08]'
                  : 'border-r border-neutral-200 dark:border-white/[0.07]'}"
              >
                {#each ['servers', 'projects'] as icon (icon)}
                  <span
                    class="flex size-6 items-center justify-center rounded-md border border-neutral-200 bg-neutral-50 text-neutral-500 dark:border-white/[0.08] dark:bg-white/[0.04] dark:text-fg-dim"
                  >
                    <Reicon name={icon} class="size-3.5" />
                  </span>
                {/each}
              </div>
              <div
                class="h-5 flex-1 bg-white dark:bg-white/[0.04] {floating
                  ? 'rounded-lg border border-neutral-200 dark:border-white/[0.08]'
                  : 'border-b border-neutral-200 dark:border-white/[0.07]'}"
              ></div>
            </div>
            {@render footer(o.label, o.description, layout.navStyle === o.value)}
          </button>
        {/each}
      </div>
    </section>

    <section class="application-settings-section">
      <div class="application-settings-section-header">
        <div>
          <h2>Style</h2>
          <p>Choose how buttons and sidebar items look.</p>
        </div>
      </div>
      <div class="application-settings-section-body grid gap-3 sm:grid-cols-2">
        {#each navItems as o (o.value)}
          {@const tiles = o.value === '3d'}
          <button type="button" onclick={() => layout.setNavItems(o.value)} class="{card} {layout.navItems === o.value ? selected : ''}">
            <div class="flex h-20 flex-col justify-center gap-1.5 border-b border-neutral-200 bg-neutral-50 px-4 dark:border-white/[0.07] dark:bg-black/15">
              {#each ['servers', 'projects'] as icon, i (icon)}
                <div
                  class="flex items-center gap-2 rounded-md text-neutral-500 dark:text-fg-dim {!tiles ? 'px-1.5 py-1' : ''} {!tiles && i === 0
                    ? 'bg-neutral-200/70 dark:bg-white/[0.08]'
                    : ''}"
                >
                  <span
                    class="flex items-center justify-center {tiles
                      ? 'size-6 rounded-md border border-neutral-200 bg-white shadow-[0_2px_0_rgb(0_0_0/0.22)] dark:border-white/[0.09] dark:bg-white/[0.04] dark:shadow-[0_2px_0_rgb(255_255_255/0.08)]'
                      : 'size-5'}"
                  >
                    <Reicon name={icon} class="size-3.5" />
                  </span>
                  <span class="h-1.5 w-16 rounded-full bg-neutral-200 dark:bg-white/10"></span>
                </div>
              {/each}
            </div>
            {@render footer(o.label, o.description, layout.navItems === o.value)}
          </button>
        {/each}
      </div>
    </section>
  </div>
</div>
