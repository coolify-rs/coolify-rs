<script lang="ts">
  // A server page coolify-rs does not implement yet, inside the server frame
  // (the sidebar keeps the server menu).
  import Empty from '../components/Empty.svelte';
  import ServerLayout from '../components/ServerLayout.svelte';
  import Unported from './Unported.svelte';
  import { loadServerContext } from '../lib/serverPage.svelte';
  import { serverSection } from '../lib/serverMenu';

  let { uuid, section }: { uuid: string; section: string } = $props();
  let name = $state<string | null>(null);
  let missing = $state(false);

  $effect(() => {
    loadServerContext(uuid, serverSection(section)).then((ctx) => {
      if (!ctx) missing = true;
      else name = ctx.facts.name;
    });
  });
</script>

{#if missing}
  <div class="application-settings-form w-full">
    <Empty title="Server not found" description="It may have been deleted, or it belongs to another team." iconName="servers" size="lg" />
  </div>
{:else if name !== null}
  <ServerLayout {name}><Unported /></ServerLayout>
{/if}
