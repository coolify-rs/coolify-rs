<script lang="ts">
  // coolify-rs's own (ADR 0010, ADR 0013): Team → Plugins. Owners and admins switch an installed plugin on
  // for their team; members see what is on. A plugin holding `deploy` also needs the team's own answer on
  // whether it may queue the team's deployments (ADR 0011). A plugin with settings has a Configure dialog
  // (ADR 0014). Built from the team pages' parts.
  import Callout from '../components/Callout.svelte';
  import CapabilityList from '../components/CapabilityList.svelte';
  import Checkbox from '../components/Checkbox.svelte';
  import Empty from '../components/Empty.svelte';
  import Modal from '../components/Modal.svelte';
  import PluginSettingsModal from '../components/PluginSettingsModal.svelte';
  import SettingsSection from '../components/SettingsSection.svelte';
  import TeamLayout from '../components/TeamLayout.svelte';
  import { ApiError } from '../lib/api';
  import { router } from '../lib/router.svelte';
  import { notify } from '../lib/toast.svelte';
  import { pluginNav, pluginsUi, type InstalledPlugin, type TeamData } from '../lib/plugins.svelte';

  let data = $state<TeamData | null>(null);
  let busy = $state<string | null>(null);
  /** A plugin holding `deploy` waiting for the team's answer before it is switched on. */
  let asking = $state<InstalledPlugin | null>(null);
  /** The plugin whose settings dialog is open. */
  let configuring = $state<InstalledPlugin | null>(null);

  async function load() {
    try {
      data = await pluginsUi.team();
    } catch (e) {
      // Plugins switched off: the page does not exist (`Route::any`).
      if (e instanceof ApiError && e.status === 404) return router.go('/', true);
      throw e;
    }
  }
  load();

  async function save(p: InstalledPlugin, enabled: boolean, grantDeploy?: boolean, message?: string) {
    busy = p.id;
    try {
      await pluginsUi.setTeam(p.id, enabled, grantDeploy);
      notify.success(message ?? (enabled ? `${p.name} is on for this team.` : `${p.name} is off for this team.`));
    } catch (e) {
      notify.failure(e instanceof ApiError ? e.message : String(e));
    } finally {
      busy = null;
      await Promise.all([load(), pluginNav.load()]);
    }
  }

  function toggle(p: InstalledPlugin, enabled: boolean) {
    if (enabled && p.deploy) {
      asking = p;
      // The checkbox stays off until the team answered.
      load();
      return;
    }
    save(p, enabled);
  }

  function answer(grant: boolean) {
    const p = asking;
    asking = null;
    if (p) save(p, true, grant);
  }
</script>

<div>
  <TeamLayout>
    {#if data}
      <div class="application-settings-form flex min-w-0 flex-col gap-6">
        <SettingsSection id="team-plugins-section" title="Plugins" description="Plugins an instance admin installed. A plugin receives this team's events and data only while it is on for the team.">
          {#if !data.plugins.length}
            <Empty title="No plugins available" description="An instance admin has not installed any plugin yet." iconName="browser-code" size="sm" />
          {:else}
            <div class="flex flex-col gap-4">
              {#each data.plugins as p (p.id)}
                <div class="rounded-lg border border-neutral-200 p-4 dark:border-white/[0.08]" data-plugin={p.id}>
                  <div class="flex flex-wrap items-start justify-between gap-3">
                    <div class="min-w-0">
                      <div class="flex flex-wrap items-center gap-2">
                        <h4 class="text-[13px] font-semibold text-black dark:text-fg">{p.name}</h4>
                        <span class="table-badge">v{p.version}</span>
                        {#if !p.verified}
                          <span class="table-badge table-badge-warning" title="Installed outside the plugin registry">Unverified</span>
                        {/if}
                        {#if p.needs_configuration}
                          <span class="table-badge table-badge-warning" title="A required setting is not set">Needs configuration</span>
                        {/if}
                        {#if p.deploy && p.enabled_for_team}
                          {#if p.deploy_granted_for_team}
                            <span class="table-badge table-badge-warning">Can deploy</span>
                          {:else}
                            <span class="table-badge">Deployments not allowed</span>
                          {/if}
                        {/if}
                      </div>
                      <p class="mt-0.5 text-[12px] text-neutral-500 dark:text-fg-dim">{p.description}</p>
                      <p class="mt-0.5 text-[11px] text-neutral-500 dark:text-fg-faint">{p.id}</p>
                    </div>
                    <div class="flex shrink-0 flex-wrap items-center gap-2">
                      {#if p.settings?.length}
                        <button type="button" class="button" onclick={() => (configuring = p)}>{data.can_manage ? 'Configure' : 'Settings'}</button>
                      {/if}
                      <Checkbox
                        id="team-plugin-{p.id}"
                        label="Enabled for this team"
                        helper={data.can_manage ? undefined : 'Only team owners and admins can change this.'}
                        checked={p.enabled_for_team}
                        disabled={!data.can_manage || busy === p.id}
                        onchange={(v) => toggle(p, v)}
                      />
                    </div>
                  </div>
                  <div class="mt-3">
                    <h5 class="mb-1.5 text-[12px] font-medium text-neutral-700 dark:text-fg-dim">Capabilities</h5>
                    <CapabilityList capabilities={p.capabilities} />
                  </div>
                  {#if p.deploy && p.enabled_for_team}
                    <div class="mt-4 flex flex-wrap items-center justify-between gap-3 border-t border-neutral-200 pt-3 dark:border-white/[0.08]">
                      <p class="text-[12px] text-neutral-600 dark:text-fg-dim">
                        {#if p.deploy_granted_for_team}
                          This plugin may queue deployments of this team's applications.
                        {:else}
                          This plugin is on for this team, but may not queue its deployments.
                        {/if}
                      </p>
                      {#if data.can_manage}
                        {#if p.deploy_granted_for_team}
                          <button type="button" class="button" disabled={busy === p.id} onclick={() => save(p, true, false, 'Deployments withdrawn.')}>Withdraw deployments</button>
                        {:else}
                          <button type="button" class="button" disabled={busy === p.id} onclick={() => (asking = p)}>Allow deployments</button>
                        {/if}
                      {/if}
                    </div>
                  {/if}
                </div>
              {/each}
            </div>
          {/if}
        </SettingsSection>
      </div>
    {/if}
  </TeamLayout>
</div>

{#if configuring && data}
  <PluginSettingsModal
    plugin={{ id: configuring.id, name: configuring.name, settings: configuring.settings ?? [] }}
    canManage={data.can_manage}
    onclose={() => (configuring = null)}
    onsaved={load}
  />
{/if}

{#if asking}
  <Modal title="Allow {asking.name} to deploy?" subtitle={asking.id} onclose={() => (asking = null)} narrow>
    <Callout type="warning" title="Deployments" class="mb-4">
      This plugin can queue deployments of this team's applications. Allow it only if you expect the plugin to deploy. You can enable it without deployments and allow them later.
    </Callout>
    <CapabilityList capabilities={asking.capabilities} />
    {#snippet footer()}
      <button type="button" class="button" onclick={() => (asking = null)}>Cancel</button>
      <button type="button" class="button" onclick={() => answer(false)}>{asking?.enabled_for_team ? 'Keep without deployments' : 'Enable without deployments'}</button>
      <button type="button" class="button button-highlighted" onclick={() => answer(true)}>{asking?.enabled_for_team ? 'Allow deployments' : 'Enable and allow deployments'}</button>
    {/snippet}
  </Modal>
{/if}
