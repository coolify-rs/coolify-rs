<script lang="ts">
  // Port of upstream livewire/team/index.blade.php. coolify-rs has no team
  // avatar upload or team creation, so those controls explain that instead.
  import FormInput from '../components/FormInput.svelte';
  import Listbox from '../components/Listbox.svelte';
  import UnsavedBar from '../components/UnsavedBar.svelte';
  import SettingsSection from '../components/SettingsSection.svelte';
  import TeamLayout from '../components/TeamLayout.svelte';
  import { teamUi, ApiError, type TeamPageData } from '../lib/api';
  import { session } from '../lib/session.svelte';
  import { notify } from '../lib/toast.svelte';

  type Form = Omit<TeamPageData['team'], 'id' | 'description'> & { description: string };

  let data = $state<TeamPageData | null>(null);
  let form = $state<Form | null>(null);
  let saved = $state<Form | null>(null);
  let saving = $state(false);
  let error = $state<ApiError | null>(null);

  async function load() {
    const d = await teamUi.get();
    data = d;
    const f = { ...d.team, description: d.team.description ?? '' };
    saved = { ...f };
    form = { ...f };
  }
  load();

  const dirty = $derived(!!form && !!saved && JSON.stringify(form) !== JSON.stringify(saved));
  const canUpdate = $derived(!!data?.can_update);

  async function save() {
    if (!form) return;
    saving = true;
    error = null;
    try {
      const { name, description, is_mcp_server_enabled, is_build_server_fallback_enabled, is_two_factor_required } = form;
      await teamUi.update({ name, description, is_mcp_server_enabled, is_build_server_fallback_enabled, is_two_factor_required });
      notify.ok('Team updated.');
      await Promise.all([load(), session.refresh()]);
    } catch (e) {
      error = e instanceof ApiError ? e : new ApiError(0, String(e));
      if (!Object.keys(error.errors).length) {
        notify.error(error.message);
        if (form && saved) form.is_two_factor_required = saved.is_two_factor_required;
      }
    } finally {
      saving = false;
    }
  }
</script>

<div>
  <TeamLayout>
    {#if form && saved && data}
      <div class="flex flex-col gap-6">
        <form
          onsubmit={(e) => {
            e.preventDefault();
            save();
          }}
          class="application-settings-form"
        >
          <UnsavedBar {dirty} {saving} onsave={save} onreset={() => ((form = { ...saved! }), (error = null))} />
          <SettingsSection title="General" description="Manage this team's identity and shared API access.">
            {#snippet actions()}
              <div class="relative h-auto w-auto">
                <button type="button" class="button" onclick={() => notify.info('Not available in coolify-rs yet', 'Create teams in Coolify for now.')}>
                  New team
                </button>
              </div>
            {/snippet}
            <div class="grid gap-4 lg:grid-cols-2">
              <div class="lg:col-span-2">
                <div class="mb-2 text-sm font-medium leading-4">Avatar</div>
                <div class="flex items-center gap-4">
                  <span
                    class="flex shrink-0 items-center justify-center rounded-md bg-neutral-200 font-semibold text-neutral-700 dark:bg-white/[0.1] dark:text-fg size-16 rounded-xl! text-2xl"
                  >
                    {saved.name.slice(0, 1).toUpperCase()}
                  </span>
                  {#if canUpdate}
                    <div class="flex min-w-0 flex-1 flex-col gap-2">
                      <p class="text-xs text-neutral-500 dark:text-fg-dim">Team avatars are not available in coolify-rs yet.</p>
                    </div>
                  {/if}
                </div>
              </div>
              <FormInput id="name" label="Name" required disabled={!canUpdate} bind:value={form.name} error={error?.field('name')} />
              <FormInput id="description" label="Description" disabled={!canUpdate} bind:value={form.description} error={error?.field('description')} />
              <div class="lg:col-span-2">
                <Listbox
                  id="is_mcp_server_enabled"
                  label="MCP server"
                  helper="Controls whether this team's API tokens can use the instance MCP endpoint."
                  disabled={!canUpdate}
                  bind:value={form.is_mcp_server_enabled}
                  options={[
                    { value: false, label: 'Disabled for this team' },
                    { value: true, label: 'Enabled for this team' },
                  ]}
                />
              </div>
              <div class="lg:col-span-2">
                <Listbox
                  id="is_build_server_fallback_enabled"
                  label="Build server fallback"
                  helper="Controls what happens when an application is configured to use a dedicated build server, but no usable build server is available."
                  disabled={!canUpdate}
                  bind:value={form.is_build_server_fallback_enabled}
                  options={[
                    { value: true, label: 'Build on the deployment server' },
                    { value: false, label: 'Fail the deployment' },
                  ]}
                />
              </div>
              <div class="lg:col-span-2">
                <Listbox
                  id="is_two_factor_required"
                  label="Two-factor authentication"
                  helper="When required, members without two-factor authentication must configure it before they can use this team."
                  disabled={!canUpdate}
                  bind:value={form.is_two_factor_required}
                  options={[
                    { value: false, label: 'Optional for members' },
                    { value: true, label: 'Required for all members' },
                  ]}
                />
              </div>
            </div>
          </SettingsSection>
        </form>
      </div>
    {/if}
  </TeamLayout>
</div>
