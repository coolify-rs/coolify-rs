<script lang="ts">
  // Port of upstream components/table/row-action.blade.php: one item of <RowActions>.
  // A link (through the router) when `href` is set, else a button.
  import type { Snippet } from 'svelte';
  import { link } from '../lib/router.svelte';

  let {
    href,
    class: klass = '',
    onclick,
    disabled = false,
    title,
    ariaLabel,
    children,
  }: {
    href?: string;
    class?: string;
    onclick?: (e: MouseEvent) => void;
    disabled?: boolean;
    title?: string;
    ariaLabel?: string;
    children: Snippet;
  } = $props();
</script>

{#if href}
  <a {href} use:link role="menuitem" {title} aria-label={ariaLabel} class="listbox-option justify-start! {klass}">{@render children()}</a>
{:else}
  <button type="button" role="menuitem" {title} aria-label={ariaLabel} {disabled} class="listbox-option justify-start! {klass}" {onclick}>{@render children()}</button>
{/if}
