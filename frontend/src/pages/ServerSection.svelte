<script lang="ts">
  // The pages under /server/{uuid}/{section}: the ported page, or the "not
  // available" one inside the server frame (the sidebar keeps the server menu).
  import type { Component } from 'svelte';
  import ServerFrame from '../components/ServerFrame.svelte';
  import Unported from './Unported.svelte';
  import ServerAdvanced from './ServerAdvanced.svelte';
  import ServerRegistries from './ServerRegistries.svelte';
  import ServerAnalytics from './ServerAnalytics.svelte';
  import ServerCharts from './ServerCharts.svelte';
  import ServerDockerCleanup from './ServerDockerCleanup.svelte';
  import ServerImages from './ServerImages.svelte';
  import ServerLogDrains from './ServerLogDrains.svelte';
  import ServerSentinel from './ServerSentinel.svelte';
  import ServerSentinelLogs from './ServerSentinelLogs.svelte';
  import ServerProxy from './ServerProxy.svelte';
  import ServerProxyDynamic from './ServerProxyDynamic.svelte';
  import ServerProxyCertificates from './ServerProxyCertificates.svelte';
  import ServerLogsPage from './ServerLogsPage.svelte';
  import ServerCaCertificate from './ServerCaCertificate.svelte';
  import ServerCloudProviderToken from './ServerCloudProviderToken.svelte';
  import ServerDestinations from './ServerDestinations.svelte';
  import ServerPrivateKey from './ServerPrivateKey.svelte';
  import ServerResources from './ServerResources.svelte';
  import ServerSwarm from './ServerSwarm.svelte';
  import ServerTerminalAccess from './ServerTerminalAccess.svelte';
  import ServerPatches from './ServerPatches.svelte';
  import ServerCloudflareTunnel from './ServerCloudflareTunnel.svelte';
  import ServerGithubRunners from './ServerGithubRunners.svelte';
  import { serverSection } from '../lib/serverMenu';

  let { uuid, section, sub }: { uuid: string; section: string; sub?: string } = $props();

  const pages: Record<string, Component<{ uuid: string; sub?: string }>> = {
    advanced: ServerAdvanced,
    registries: ServerRegistries,
    analytics: ServerAnalytics,
    metrics: ServerCharts,
    'docker-cleanup': ServerDockerCleanup,
    images: ServerImages,
    'log-drains': ServerLogDrains,
    sentinel: ServerSentinel,
    'sentinel/logs': ServerSentinelLogs,
    proxy: ServerProxy,
    'proxy/dynamic': ServerProxyDynamic,
    'proxy/certificates': ServerProxyCertificates,
    'proxy/logs': ServerLogsPage,
    'ca-certificate': ServerCaCertificate,
    'cloud-provider-token': ServerCloudProviderToken,
    destinations: ServerDestinations,
    'private-key': ServerPrivateKey,
    resources: ServerResources,
    swarm: ServerSwarm,
    'security/terminal-access': ServerTerminalAccess,
    'security/patches': ServerPatches,
    'cloudflare-tunnel': ServerCloudflareTunnel,
    'github-runners': ServerGithubRunners,
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
