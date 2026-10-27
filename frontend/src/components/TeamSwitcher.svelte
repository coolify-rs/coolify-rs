<script lang="ts">
  // Port of upstream livewire/switch-team.blade.php (expanded variant).
  import Reicon from './Reicon.svelte';
  import { session } from '../lib/session.svelte';
  import { sessionApi } from '../lib/api';
  import { router } from '../lib/router.svelte';
  import { notify } from '../lib/toast.svelte';

  let open = $state(false);
  let root: HTMLDivElement | undefined = $state();
  const current = $derived(session.info?.current_team);
  const initial = (name: string) => name.slice(0, 1).toUpperCase();

  async function switchTo(id: number) {
    open = false;
    if (id === current?.id) return;
    try {
      session.set(await sessionApi.switchTeam(id));
      router.go('/');
    } catch (e) {
      notify.error('Could not switch team', e instanceof Error ? e.message : String(e));
    }
  }

  function newTeam() {
    open = false;
    notify.info('Not available in coolify-rs yet', 'Create teams in Coolify for now.');
  }
</script>

<svelte:window
  onclick={(e) => open && root && !root.contains(e.target as Node) && (open = false)}
  onkeydown={(e) => e.key === 'Escape' && (open = false)}
/>

<div class="min-w-0">
  <div class="relative min-w-0" bind:this={root}>
    <button
      type="button"
      onclick={() => (open = !open)}
      title="Switch team"
      class="group/team flex h-8 items-center gap-1.5 rounded-lg px-2 -ml-1 text-left opacity-70 transition-[background-color,opacity] hover:bg-neutral-100 hover:opacity-100 dark:hover:bg-white/[0.05]"
    >
      <span
        class="flex shrink-0 items-center justify-center rounded-md bg-neutral-200 font-semibold text-neutral-700 dark:bg-white/[0.1] dark:text-fg size-5 text-[11px]"
      >
        {initial(current?.name ?? '')}
      </span>
      <span class="whitespace-nowrap text-[13px] font-semibold text-black dark:text-fg">{current?.name ?? ''}</span>
      <svg class="size-4 shrink-0 text-neutral-400 dark:text-fg-faint" viewBox="0 0 24 24" fill="none">
        <path d="M8 9l4-4 4 4M8 15l4 4 4-4" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" />
      </svg>
    </button>
    {#if open}
      <div class="listbox-panel left-0! z-[90]! max-h-72! min-w-56">
        <div class="px-2 py-1 text-[10px] font-semibold uppercase tracking-wide text-neutral-400 dark:text-fg-faint">Teams</div>
        {#each session.info?.teams ?? [] as team (team.id)}
          <button
            type="button"
            onclick={() => switchTo(team.id)}
            class="listbox-option {team.id === current?.id ? 'bg-neutral-100 font-medium dark:bg-white/[0.06]' : ''}"
          >
            <span
              class="flex shrink-0 items-center justify-center rounded-md bg-neutral-200 font-semibold text-neutral-700 dark:bg-white/[0.1] dark:text-fg size-5 text-[11px]"
            >
              {initial(team.name)}
            </span>
            <span class="min-w-0 flex-1 truncate">{team.name}</span>
          </button>
        {/each}
        <div class="mt-1 border-t border-neutral-200 pt-1 dark:border-white/[0.08]">
          <button type="button" class="listbox-option w-full" onclick={newTeam}>
            <Reicon name="plus" class="size-3.5 shrink-0" />
            <span class="min-w-0 flex-1 text-left">New team</span>
          </button>
        </div>
      </div>
    {/if}
  </div>
</div>
