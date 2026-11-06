<script lang="ts">
  // coolify-rs's own (ADR 0013): the plugin panels of one slot, each under a heading in the style of the
  // page it sits on (the dashboard's section headings, or a settings section elsewhere). Renders nothing
  // when the team has no panel in the slot.
  import PluginPanel from './PluginPanel.svelte';
  import SectionHeading from './SectionHeading.svelte';
  import SettingsSection from './SettingsSection.svelte';
  import { pluginNav, type Slot } from '../lib/plugins.svelte';

  let { slot, resource = null }: { slot: Slot; resource?: string | null } = $props();

  const panels = $derived(pluginNav.of(slot));
</script>

{#snippet unverified(verified: boolean)}
  {#if !verified}
    <span class="table-badge table-badge-warning" title="Installed outside the plugin registry">Unverified</span>
  {/if}
{/snippet}

{#each panels as p (`${p.plugin}/${p.index}`)}
  {#if slot === 'dashboard'}
    <section class="@container mb-0! min-w-0" data-plugin={p.plugin}>
      <SectionHeading title={p.title} subtitle={p.name}>
        {#snippet titleAction()}{@render unverified(p.verified)}{/snippet}
      </SectionHeading>
      <PluginPanel panel={p} />
    </section>
  {:else}
    <SettingsSection title={p.title} description="Shown by the {p.name} plugin.">

      {#snippet badges()}{@render unverified(p.verified)}{/snippet}
      <PluginPanel panel={p} {resource} />
    </SettingsSection>
  {/if}
{/each}
