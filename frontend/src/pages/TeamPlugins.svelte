<script lang="ts">
  // coolify-rs's own (ADR 0010, ADR 0013): Team → Plugins. Owners and admins switch an installed plugin on
  // for their team; members see what is on. Built from the team pages' parts.
  import CapabilityList from '../components/CapabilityList.svelte';
  import Checkbox from '../components/Checkbox.svelte';
  import Empty from '../components/Empty.svelte';
  import SettingsSection from '../components/SettingsSection.svelte';
  import TeamLayout from '../components/TeamLayout.svelte';
  import { ApiError } from '../lib/api';
  import { router } from '../lib/router.svelte';
  import { notify } from '../lib/toast.svelte';
  import { pluginNav, pluginsUi, type InstalledPlugin, type TeamData } from '../lib/plugins.svelte';

  let data = $state<TeamData | null>(null);
  let busy = $state<string | null>(null);

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

  async function toggle(p: InstalledPlugin, enabled: boolean) {
    busy = p.id;
    try {
      await pluginsUi.setTeam(p.id, enabled);
      notify.success(enabled ? `${p.name} is on for this team.` : `${p.name} is off for this team.`);
    } catch (e) {
      notify.failure(e instanceof ApiError ? e.message : String(e));
    } finally {
      busy = null;
      await Promise.all([load(), pluginNav.load()]);
    }
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
                      </div>
                      <p class="mt-0.5 text-[12px] text-neutral-500 dark:text-fg-dim">{p.description}</p>
                      <p class="mt-0.5 text-[11px] text-neutral-500 dark:text-fg-faint">{p.id}</p>
                    </div>
                    <div class="shrink-0">
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
                </div>
              {/each}
            </div>
          {/if}
        </SettingsSection>
      </div>
    {/if}
  </TeamLayout>
</div>
