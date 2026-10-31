<script lang="ts">
  // The pages under /server/{uuid}/{section}: the ported page, or the "not
  // available" one inside the server frame (the sidebar keeps the server menu).
  import type { Component } from 'svelte';
  import ServerFrame from '../components/ServerFrame.svelte';
  import Unported from './Unported.svelte';
  import ServerAdvanced from './ServerAdvanced.svelte';
  import ServerCaCertificate from './ServerCaCertificate.svelte';
  import ServerDestinations from './ServerDestinations.svelte';
  import ServerPrivateKey from './ServerPrivateKey.svelte';
  import ServerResources from './ServerResources.svelte';
  import ServerSwarm from './ServerSwarm.svelte';
  import ServerTerminalAccess from './ServerTerminalAccess.svelte';
  import { serverSection } from '../lib/serverMenu';

  let { uuid, section, sub }: { uuid: string; section: string; sub?: string } = $props();

  const pages: Record<string, Component<{ uuid: string; sub?: string }>> = {
    advanced: ServerAdvanced,
    'ca-certificate': ServerCaCertificate,
    destinations: ServerDestinations,
    'private-key': ServerPrivateKey,
    resources: ServerResources,
    swarm: ServerSwarm,
    'security/terminal-access': ServerTerminalAccess,
  };
  const Page = $derived(pages[sub ? `${section}/${sub}` : section] ?? pages[section]);
</script>

{#if Page}
  <Page {uuid} {sub} />
{:else}
  <ServerFrame {uuid} active={serverSection(section)} title={section}>
    {#snippet children()}
      <Unported />
    {/snippet}
  </ServerFrame>
{/if}
