<script lang="ts">
  // Port of upstream livewire/team/danger-zone.blade.php (App\Livewire\Team\DangerZone).
  import { untrack } from 'svelte';
  import Reicon from '../components/Reicon.svelte';
  import TeamLayout from '../components/TeamLayout.svelte';
  import SettingsSection from '../components/SettingsSection.svelte';
  import DangerZone from '../components/DangerZone.svelte';
  import ConfirmModal from '../components/ConfirmModal.svelte';
  import { teamsUi, type DangerData } from '../lib/teamsApi';
  import { link, router } from '../lib/router.svelte';
  import { session } from '../lib/session.svelte';

  let data = $state<DangerData | null>(null);
  let deleting = $state(false);

  async function load() {
    try {
      data = await teamsUi.danger();
    } catch {
      router.go('/', true);
    }
  }
  untrack(load);

  const owner = $derived(session.info?.current_team?.role === 'owner');
  const root = $derived(session.info?.current_team?.id === 0);
  const blockers = $derived.by(() => {
    if (!data) return [];
    const { projects, servers, sources } = data.blockers;
    const plural = (label: string, n: number) => (n === 1 ? label : `${label}s`);
    return [
      projects && { href: '/projects', text: `${projects} ${plural('project', projects)}` },
      servers && { href: '/servers', text: `${servers} ${plural('server', servers)}` },
      sources && { href: '/sources', text: `${sources} ${plural('Git source', sources)}` },
    ].filter(Boolean) as { href: string; text: string }[];
  });
  const canDelete = $derived(!!data && !root && owner && data.team_count > 1 && !data.personal_team && !data.has_subscription && blockers.length === 0);
  const showResources = $derived(!!data && !root && !data.has_subscription && (data.projects.length > 0 || data.servers.length > 0));

  async function remove(): Promise<string | void> {
    await teamsUi.remove();
    await session.refresh();
    router.go(session.info?.current_team ? '/team' : '/select-team', true);
  }
</script>

<div>
  <TeamLayout>
    {#if data}
      <div class="application-settings-form">
        <SettingsSection id="team-danger-zone" title="Danger zone" description="Destructive actions for this team cannot be undone.">
          <DangerZone title="Delete team">
            {#if !owner}
              <p>Only team owners can delete this team.</p>
            {:else if root}
              <p>The default team cannot be deleted.</p>
            {:else if data.team_count === 1 || data.personal_team}
              <p>Your last or personal team cannot be deleted.</p>
            {:else if data.has_subscription}
              <p>
                Cancel your <a class="font-medium text-coollabs hover:underline dark:text-brand-light" href="/subscription" use:link>subscription</a>
                before deleting this team.
              </p>
            {:else if blockers.length === 0}
              <p>
                Permanently delete <strong class="font-semibold text-black dark:text-fg">{data.name}</strong>
                from Coolify. This action cannot be undone.
              </p>
              <ul class="space-y-1 text-xs">
                <li>• All members will lose access to this team.</li>
                <li>• This team cannot be restored from Coolify after deletion.</li>
              </ul>
            {:else}
              <p>This team still owns:</p>
              <ul class="space-y-1">
                {#each blockers as b (b.href)}
                  <li>
                    <a class="font-medium text-coollabs hover:underline dark:text-brand-light" href={b.href} use:link>{b.text}</a>
                  </li>
                {/each}
              </ul>
              <p>Remove or move these resources before deleting the team.</p>
            {/if}
            {#snippet action()}
              {#if canDelete}
                <div class="relative h-auto max-w-full inline-flex w-auto">
                  <button type="button" class="button button-error flex gap-2" onclick={() => (deleting = true)}>Delete team</button>
                </div>
              {:else}
                <span class="relative inline-flex" title="Resolve the requirements shown before deleting this team.">
                  <button type="button" class="button button-error" disabled>Delete team</button>
                </span>
              {/if}
            {/snippet}
          </DangerZone>

          {#if showResources}
            <div class="mt-4 overflow-hidden rounded-lg border border-neutral-200 dark:border-white/[0.08]">
              <div class="flex items-center justify-between gap-3 border-b border-neutral-200 px-3 py-2 dark:border-white/[0.08]">
                <h5 class="text-sm font-medium text-black dark:text-fg">Resources</h5>
                <button type="button" class="button" onclick={load}>
                  <Reicon name="refresh" class="size-3.5" />
                  Refresh
                </button>
              </div>
              <table class="w-full text-left text-sm">
                <thead class="bg-neutral-50 text-[11px] uppercase tracking-wide text-neutral-500 dark:bg-coolgray-100 dark:text-fg-dim">
                  <tr>
                    <th class="px-3 py-2 font-medium">Resource</th>
                    <th class="px-3 py-2 font-medium">Name</th>
                  </tr>
                </thead>
                <tbody class="divide-y divide-neutral-200 dark:divide-white/[0.08]">
                  {#each data.projects as project (project.uuid)}
                    <tr class="text-[13px] text-neutral-600 hover:bg-neutral-50 dark:text-fg-dim dark:hover:bg-white/[0.03]">
                      <td><a class="block px-3 py-2.5" href="/project/{project.uuid}" target="_blank" rel="noopener noreferrer">Project</a></td>
                      <td><a class="block px-3 py-2.5 font-medium text-black dark:text-fg" href="/project/{project.uuid}" target="_blank" rel="noopener noreferrer">{project.name}</a></td>
                    </tr>
                  {/each}
                  {#each data.servers as server (server.uuid)}
                    <tr class="text-[13px] text-neutral-600 hover:bg-neutral-50 dark:text-fg-dim dark:hover:bg-white/[0.03]">
                      <td><a class="block px-3 py-2.5" href="/server/{server.uuid}" target="_blank" rel="noopener noreferrer">Server</a></td>
                      <td><a class="block px-3 py-2.5 font-medium text-black dark:text-fg" href="/server/{server.uuid}" target="_blank" rel="noopener noreferrer">{server.name}</a></td>
                    </tr>
                  {/each}
                </tbody>
              </table>
            </div>
          {/if}
        </SettingsSection>
      </div>
    {/if}
  </TeamLayout>
</div>

{#if deleting && data}
  <ConfirmModal
    title="Confirm Team Deletion?"
    actions={['The current team will be permanently deleted from Coolify and the database.']}
    confirmationText={data.name}
    confirmationLabel="Enter the team name to confirm permanent deletion"
    shortConfirmationLabel="Team name"
    confirmWithPassword={false}
    step2ButtonText="Permanently Delete"
    onconfirm={remove}
    onclose={() => (deleting = false)}
  />
{/if}
