<script lang="ts" module>
  // `mountOnPage` of upstream's canvas core registers one listener per initialize call.
  const started: Record<string, boolean> = {};
</script>

<script lang="ts">
  // The shell of a canvas page: the element upstream's canvas code in src/upstream/js mounts into
  // (`[data-environment-canvas]`, `[data-deploy-canvas]`). Their `mountOnPage` waits for Livewire's
  // `livewire:navigated`, so this sends that event when a canvas page opens or closes.
  import { onDestroy, onMount, type Snippet } from 'svelte';
  import { csrf } from '../lib/api';

  let {
    kind = 'environment',
    attrs,
    class: cls,
    children,
  }: {
    kind?: 'environment' | 'deploy';
    /** The `data-*` attributes of the Blade view. */
    attrs: Record<string, string>;
    class: string;
    children?: Snippet;
  } = $props();

  const navigated = () => document.dispatchEvent(new Event('livewire:navigated'));

  onMount(async () => {
    // The canvas API reads the token from the layout's meta tag.
    let meta = document.querySelector<HTMLMetaElement>('meta[name="csrf-token"]');
    if (!meta) {
      meta = document.createElement('meta');
      meta.name = 'csrf-token';
      document.head.append(meta);
    }
    meta.content = csrf();
    if (kind === 'deploy') {
      const m = await import('../upstream/js/canvas/deploy/index.js');
      if (!started.deploy) m.initializeDeployCanvas();
      started.deploy = true;
    } else {
      const m = await import('../upstream/js/canvas/environment/index.js');
      if (!started.environment) m.initializeEnvironmentCanvas();
      started.environment = true;
    }
    navigated();
  });

  onDestroy(() => queueMicrotask(navigated));
</script>

<div {...attrs} class={cls}>
  {@render children?.()}
</div>
