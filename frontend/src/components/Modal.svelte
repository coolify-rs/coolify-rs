<script lang="ts">
  import type { Snippet } from 'svelte';

  let {
    title,
    description,
    onclose,
    children,
    actions,
  }: {
    title: string;
    description?: string;
    onclose: () => void;
    children: Snippet;
    actions?: Snippet;
  } = $props();

  function onkeydown(e: KeyboardEvent) {
    if (e.key === 'Escape') onclose();
  }
</script>

<svelte:window {onkeydown} />

<div class="backdrop" role="presentation" onmousedown={(e) => e.target === e.currentTarget && onclose()}>
  <div class="card modal" role="dialog" aria-modal="true" aria-label={title}>
    <div class="card-header">
      <div>
        <div class="card-title">{title}</div>
        {#if description}<div class="card-description">{description}</div>{/if}
      </div>
    </div>
    <div class="card-well">
      {@render children()}
    </div>
    {#if actions}
      <div class="modal-actions">{@render actions()}</div>
    {/if}
  </div>
</div>
