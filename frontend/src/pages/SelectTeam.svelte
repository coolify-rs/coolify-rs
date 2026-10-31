<script lang="ts">
  // Port of upstream livewire/select-team.blade.php (App\Livewire\SelectTeam).
  import AuthShell from '../components/AuthShell.svelte';
  import Reicon from '../components/Reicon.svelte';
  import { sessionApi } from '../lib/api';
  import { session } from '../lib/session.svelte';
  import { router } from '../lib/router.svelte';

  // A team is already active, or there is nothing to pick: back to the dashboard.
  $effect(() => {
    if (session.info?.current_team) router.go('/', true);
  });

  async function select(id: number) {
    session.set(await sessionApi.switchTeam(id));
    router.go('/', true);
  }
</script>

<AuthShell title="Select a team" description="Choose the team you want to work in. Your choice is remembered for next time.">
  <div class="flex flex-col gap-2">
    {#each session.info?.teams ?? [] as team (team.id)}
      <button
        type="button"
        onclick={() => select(team.id)}
        class="group flex items-center gap-3 rounded-lg border border-neutral-200 px-3 py-2.5 text-left transition-colors hover:border-neutral-300 hover:bg-neutral-50 dark:border-white/[0.08] dark:hover:border-white/[0.16] dark:hover:bg-white/[0.04]"
      >
        <span class="flex size-8 shrink-0 items-center justify-center rounded-lg bg-neutral-100 text-[13px] font-semibold text-neutral-600 dark:bg-white/[0.06] dark:text-fg">
          {team.name.slice(0, 1).toUpperCase()}
        </span>
        <span class="min-w-0 flex-1 truncate text-[13px] font-semibold text-black dark:text-fg">{team.name}</span>
        <Reicon name="arrow-right" class="size-4 shrink-0 text-neutral-400 dark:text-fg-faint" />
      </button>
    {/each}
  </div>
</AuthShell>
