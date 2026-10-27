<script lang="ts">
  // Port of upstream components/application/settings-section.blade.php.
  import type { Snippet } from 'svelte';
  import Helper from './Helper.svelte';

  let {
    title,
    description,
    flush = false,
    actions,
    badges,
    children,
  }: { title: string; description?: string; flush?: boolean; actions?: Snippet; badges?: Snippet; children: Snippet } = $props();
</script>

<section class="application-settings-section" data-settings-section-title={title}>
  <header>
    <div class="min-w-0 py-0.5 {badges ? 'flex flex-wrap items-center gap-2' : ''}">
      {#if description}
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
  <div class="application-settings-section-body {flush ? 'is-flush' : ''}">
    {@render children()}
  </div>
</section>
