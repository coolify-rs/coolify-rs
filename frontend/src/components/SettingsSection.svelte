<script lang="ts">
  // Port of upstream components/application/settings-section.blade.php.
  import { untrack, type Snippet } from 'svelte';
  import Helper from './Helper.svelte';
  import Reicon from './Reicon.svelte';

  let {
    id,
    title,
    description,
    flush = false,
    collapsible = false,
    open: initiallyOpen = false,
    actions,
    badges,
    children,
    class: cls = '',
  }: {
    class?: string;
    id?: string;
    title: string;
    description?: string;
    flush?: boolean;
    /** A collapsible card shows only its title row until the user opens it. */
    collapsible?: boolean;
    open?: boolean;
    actions?: Snippet;
    badges?: Snippet;
    /** A card with nothing in its body shows only its title row. */
    children?: Snippet;
  } = $props();

  let open = $state(untrack(() => initiallyOpen));
  let section: HTMLElement | undefined = $state();
  // Upstream names the body `section-<md5 of the title>-body` when the card has no id.
  const bodyId = $derived(collapsible ? `${id || `section-${title.toLowerCase().replace(/[^a-z0-9]+/g, '-')}`}-body` : undefined);
  const hasBody = $derived(collapsible || !!children);

  // `open-settings-section`: another part of the page opens this card and scrolls to it.
  function openFromEvent(e: Event) {
    if (!section || (e as CustomEvent).detail?.id !== section.id) return;
    open = true;
    queueMicrotask(() =>
      section?.scrollIntoView({ behavior: window.matchMedia('(prefers-reduced-motion: reduce)').matches ? 'auto' : 'smooth', block: 'start' }),
    );
  }
  $effect(() => {
    if (!collapsible) return;
    window.addEventListener('open-settings-section', openFromEvent);
    return () => window.removeEventListener('open-settings-section', openFromEvent);
  });
</script>

<section bind:this={section} {id} class="application-settings-section {cls}" data-settings-section-title={title}>
  <header class={(collapsible ? !open : !hasBody) ? 'is-collapsed' : undefined}>
    <div class="min-w-0 py-0.5 {badges ? 'flex flex-wrap items-baseline gap-x-2 gap-y-1' : ''}">
      {#if collapsible}
        <h3 class={description ? 'flex items-center gap-1.5' : undefined}>
          <button type="button" class="settings-section-toggle" onclick={() => (open = !open)} aria-expanded={open ? 'true' : 'false'} aria-controls={bodyId}>
            <Reicon name="chevron-down" class="size-3.5 shrink-0 transition-transform duration-150 {open ? 'rotate-0' : '-rotate-90'}" />
            <span>{title}</span>
          </button>
          {#if description}
            <Helper helper={description} label="More information about {title}" />
          {/if}
        </h3>
      {:else if description}
        <h3>
          <Helper helper={description} label="More information about {title}">
            {#snippet trigger()}
              <span class="underline underline-offset-4">{title}</span>
            {/snippet}
          </Helper>
        </h3>
      {:else}
        <h3>{title}</h3>
      {/if}
      {@render badges?.()}
    </div>
    {#if actions}
      <div class="flex min-w-0 max-w-full flex-wrap items-center gap-2">
        {@render actions()}
      </div>
    {/if}
  </header>
  {#if hasBody}
    <div id={bodyId} style:display={collapsible && !open ? 'none' : undefined} class="application-settings-section-body {flush ? 'is-flush' : ''}">
      {@render children?.()}
    </div>
  {/if}
</section>
