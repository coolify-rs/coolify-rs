<script lang="ts">
  // The common part of a server page: loads the server (menu, top bar, title),
  // answers "not found" and wraps the page in the server layout.
  import type { Snippet } from 'svelte';
  import Empty from './Empty.svelte';
  import ServerLayout from './ServerLayout.svelte';
  import type { ServerFacts } from '../lib/api';
  import { loadServerContext } from '../lib/serverPage.svelte';

  let {
    uuid,
    active,
    title,
    onloaded,
    children,
  }: {
    uuid: string;
    active: string;
    /** The page part of the title (`{name} > Advanced | Coolify`). */
    title: string;
    onloaded?: (facts: ServerFacts) => void;
    children: Snippet<[ServerFacts]>;
  } = $props();

  let facts = $state<ServerFacts | null>(null);
  let missing = $state(false);

  /** `Str::limit($name, 10)`. */
  const limit = (s: string) => (s.length > 10 ? `${s.slice(0, 10)}...` : s);

  $effect(() => {
    loadServerContext(uuid, active).then((ctx) => {
      if (!ctx) {
        missing = true;
        return;
      }
      facts = ctx.facts;
      document.title = `${limit(ctx.facts.name)} > ${title} | coolify-rs`;
      onloaded?.(ctx.facts);
    });
  });
</script>

{#if missing}
  <div class="application-settings-form w-full">
    <Empty title="Server not found" description="It may have been deleted, or it belongs to another team." iconName="servers" size="lg" />
  </div>
{:else if facts}
  <ServerLayout name={facts.name}>
    {@render children(facts)}
  </ServerLayout>
{/if}
