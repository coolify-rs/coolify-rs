<script lang="ts">
  // Port of upstream livewire/team/index.blade.php.
  import FormInput from '../components/FormInput.svelte';
  import Listbox from '../components/Listbox.svelte';
  import UnsavedBar from '../components/UnsavedBar.svelte';
  import SettingsSection from '../components/SettingsSection.svelte';
  import TeamLayout from '../components/TeamLayout.svelte';
  import TeamAvatar from '../components/TeamAvatar.svelte';
  import { compressAvatar, teamsUi, uploadImage } from '../lib/teamsApi';
  import { teamUi, ApiError, type TeamPageData } from '../lib/api';
  import { session } from '../lib/session.svelte';
  import { notify } from '../lib/toast.svelte';

  type Form = Omit<TeamPageData['team'], 'id' | 'description' | 'avatar_url'> & { description: string };

  let data = $state<TeamPageData | null>(null);
  let form = $state<Form | null>(null);
  let saved = $state<Form | null>(null);
  let saving = $state(false);
  let error = $state<ApiError | null>(null);
  let processing = $state(false);
  let uploadError = $state<string | null>(null);
  let avatarInput: HTMLInputElement | undefined = $state();

  async function load() {
    const d = await teamUi.get();
    data = d;
    const { id: _id, avatar_url: _avatar, ...rest } = d.team;
    const f = { ...rest, description: d.team.description ?? '' };
    saved = { ...f };
    form = { ...f };
  }
  load();

  const dirty = $derived(!!form && !!saved && JSON.stringify(form) !== JSON.stringify(saved));
  const canUpdate = $derived(!!data?.can_update);

  /** `prepareAvatar` and `uploadAvatar` of the Blade view. */
  async function prepareAvatar(event: Event) {
    const input = event.target as HTMLInputElement;
    const file = input.files?.[0];
    if (!file) return;
    processing = true;
    uploadError = null;
    try {
      const compressed = await compressAvatar(file);
      try {
        const r = await uploadImage('/ui/team/avatar', compressed, session.info?.csrf_token ?? '');
        notify.success(r.message);
        await Promise.all([load(), session.refresh()]);
      } catch (e) {
        if (e instanceof ApiError) notify.failure(e.message);
        else uploadError = 'The image could not be uploaded.';
      }
    } catch {
      uploadError = 'The image could not be processed in this browser.';
    }
    processing = false;
    input.value = '';
  }

  async function removeAvatar() {
    try {
      const r = await teamsUi.removeAvatar();
      notify.success(r.message);
      await Promise.all([load(), session.refresh()]);
    } catch (e) {
      notify.failure(e instanceof ApiError ? e.message : String(e));
    }
  }

  async function save() {
    if (!form) return;
    saving = true;
    error = null;
    try {
      const { name, description, is_mcp_server_enabled, is_build_server_fallback_enabled, is_two_factor_required } = form;
      await teamUi.update({ name, description, is_mcp_server_enabled, is_build_server_fallback_enabled, is_two_factor_required });
      notify.success('Team updated.');
      await Promise.all([load(), session.refresh()]);
    } catch (e) {
      error = e instanceof ApiError ? e : new ApiError(0, String(e));
      if (!Object.keys(error.errors).length) {
        notify.failure(error.message);
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
            <div class="grid gap-4 lg:grid-cols-2">
              <div class="lg:col-span-2">
                <div class="mb-2 text-sm font-medium leading-4">Avatar</div>
                <div class="flex items-center gap-4">
                  <TeamAvatar name={saved.name} url={data.team.avatar_url} class="size-16 rounded-xl! text-2xl" />
                  {#if canUpdate}
                    <div class="flex min-w-0 flex-1 flex-col gap-2">
                      <div class="flex flex-wrap gap-2">
                        <input bind:this={avatarInput} type="file" onchange={prepareAvatar} accept="image/jpeg,image/png,image/webp" class="hidden" />
                        <button type="button" class="button" disabled={processing} onclick={() => avatarInput?.click()}>
                          <span>{processing ? 'Uploading…' : 'Browse…'}</span>
                        </button>
                        {#if data.team.avatar_url}
                          <button type="button" class="button button-error" disabled={processing} onclick={removeAvatar}>Remove</button>
                        {/if}
                      </div>
                      <p class="text-xs text-neutral-500 dark:text-fg-dim">JPG, PNG, or WebP. Shown in the team switcher.</p>
                      {#if uploadError}<p class="text-xs text-red-500">{uploadError}</p>{/if}
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

