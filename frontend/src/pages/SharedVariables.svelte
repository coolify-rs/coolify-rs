<script lang="ts">
  // Port of upstream livewire/shared-variables/index.blade.php.
  import Reicon from '../components/Reicon.svelte';
  import SharedVariablesLayout from '../components/SharedVariablesLayout.svelte';
  import { sharedVariablesUi, type Overview, type OverviewRow } from '../lib/sharedVariablesApi';
  import { link } from '../lib/router.svelte';

  document.title = 'Shared Variables | coolify-rs';

  let overview = $state<Overview | null>(null);
  sharedVariablesUi.overview().then((o) => (overview = o));

  const scopeCards: [string, string, string, string, string, string | null][] = [
    ['team', 'Team wide', 'teams', '/shared-variables/team', 'Every resource of this team.', null],
    ['project', 'Project wide', 'projects', '/shared-variables/projects', 'Every environment of a project.', 'project'],
    ['environment', 'Environment wide', 'layers', '/shared-variables/environments', 'Resources in one environment.', 'environment'],
    ['server', 'Server wide', 'servers', '/shared-variables/servers', 'Resources deployed on one server.', 'server'],
  ];
  const panel = 'rounded-xl border border-neutral-200 bg-white shadow-sm dark:border-white/[0.08] dark:bg-white/[0.05]';
  const panelTitle =
    'flex items-center justify-between gap-2 border-b border-neutral-200 px-3 py-2.5 text-[12px] font-semibold text-black dark:border-white/[0.06] dark:text-fg';
  const row =
    'flex items-center gap-2 border-b border-neutral-100 px-3 py-2 last:border-b-0 hover:bg-neutral-50 hover:no-underline dark:border-white/[0.04] dark:hover:bg-white/[0.03]';
  const scopeBadge =
    'shrink-0 rounded-full border border-neutral-200 bg-neutral-50 px-1.5 text-[10px] leading-4 font-medium text-neutral-500 dark:border-white/[0.1] dark:bg-white/[0.05] dark:text-fg-dim';

  const plural = (word: string, n: number) => (n === 1 ? word : word === 'reference' ? 'references' : `${word}s`);
  const headline = (s: string) => s.charAt(0).toUpperCase() + s.slice(1);

  /** Carbon `diffForHumans(short: true)`. */
  function ago(iso: string | null): string {
    if (!iso) return '';
    const seconds = Math.max(0, Math.floor((Date.now() - new Date(iso + (iso.endsWith('Z') ? '' : 'Z')).getTime()) / 1000));
    const units: [number, string][] = [
      [31536000, 'yr'],
      [2592000, 'mo'],
      [604800, 'w'],
      [86400, 'd'],
      [3600, 'h'],
      [60, 'm'],
    ];
    for (const [size, unit] of units) if (seconds >= size) return `${Math.floor(seconds / size)}${unit} ago`;
    return `${seconds}s ago`;
  }
</script>

{#snippet key(v: OverviewRow)}
  <span class="min-w-0 flex-1">
    <span class="block truncate font-mono text-[12px] text-black dark:text-fg">{v.key}</span>
    <span class="block truncate text-[11px] text-neutral-500 dark:text-fg-faint">{v.owner}</span>
  </span>
{/snippet}

<div>
  <SharedVariablesLayout>
    {#if overview}
      <div class="flex w-full flex-col gap-4">
        <div class="grid grid-cols-1 gap-3 sm:grid-cols-2 lg:grid-cols-4">
          {#each scopeCards as [scope, label, icon, href, description, ownerNoun] (scope)}
            {@const counts = overview.scopes[scope]}
            <a
              use:link
              {href}
              class="group flex min-h-32 flex-col rounded-xl border border-neutral-200 bg-white p-3 shadow-sm transition-all hover:-translate-y-px hover:border-neutral-300 hover:no-underline hover:shadow-md dark:border-white/[0.08] dark:bg-white/[0.05] dark:hover:border-white/[0.14]"
            >
              <div class="flex items-start justify-between gap-2">
                <div class="flex size-8 items-center justify-center rounded-lg border border-neutral-200 bg-neutral-50 text-neutral-500 dark:border-white/[0.08] dark:bg-white/[0.04] dark:text-fg-dim">
                  <Reicon name={icon} class="size-4" />
                </div>
                <span class="text-[22px] leading-7 font-semibold tabular-nums text-black dark:text-fg">{counts.variables}</span>
              </div>
              <div class="mt-auto pt-5">
                <h2 class="text-[13px]! leading-4! font-semibold! text-black dark:text-fg">{label}</h2>
                <p class="mt-1 text-[11px] leading-4 text-neutral-500 dark:text-fg-faint">
                  {#if ownerNoun && counts.variables > 0}
                    {plural('variable', counts.variables)} on {counts.owners} {plural(ownerNoun, counts.owners)}.
                  {:else}
                    {description}
                  {/if}
                </p>
              </div>
            </a>
          {/each}
        </div>

        {#if overview.missing > 0}
          <div class="flex items-start gap-2 rounded-xl border border-warning/30 bg-warning/10 px-3 py-2.5 text-[12px] text-neutral-700 dark:text-fg-dim" role="note">
            <Reicon name="alert-triangle" class="mt-px size-4 shrink-0 text-warning" />
            <p>
              {overview.missing} {plural('reference', overview.missing)} in resource variables
              {overview.missing === 1 ? 'points' : 'point'} to a shared variable that does not exist. Open a scope canvas to find
              {overview.missing === 1 ? 'it' : 'them'}.
            </p>
          </div>
        {/if}

        <div class="grid grid-cols-1 gap-3 lg:grid-cols-3">
          <section class={panel}>
            <h2 class={panelTitle}>Most used</h2>
            {#each overview.used as v (v.link)}
              <a class={row} href={v.link}>
                {@render key(v)}
                <span class="shrink-0 text-[11px] text-neutral-500 tabular-nums dark:text-fg-dim">{v.resources} {plural('resource', v.resources)}</span>
              </a>
            {:else}
              <p class="px-3 py-4 text-[12px] text-neutral-500 dark:text-fg-faint">No resource uses a shared variable yet.</p>
            {/each}
          </section>

          <section class={panel}>
            <h2 class={panelTitle}>Recently changed</h2>
            {#each overview.recent as v (v.link)}
              <a class={row} href={v.link}>
                {@render key(v)}
                <span class="shrink-0 text-[11px] text-neutral-500 dark:text-fg-dim">{ago(v.updated_at)}</span>
              </a>
            {:else}
              <p class="px-3 py-4 text-[12px] text-neutral-500 dark:text-fg-faint">No shared variables yet.</p>
            {/each}
          </section>

          <section class={panel}>
            <h2 class={panelTitle}>
              Not used
              <span class="text-[11px] font-normal text-neutral-400 tabular-nums dark:text-fg-faint">{overview.unused.length}</span>
            </h2>
            <div class="max-h-72 overflow-y-auto">
              {#each overview.unused as v (v.link)}
                <a class={row} href={v.link}>
                  {@render key(v)}
                  <span class={scopeBadge}>{headline(v.scope)}</span>
                </a>
              {:else}
                <p class="px-3 py-4 text-[12px] text-neutral-500 dark:text-fg-faint">Every shared variable is in use.</p>
              {/each}
            </div>
          </section>
        </div>
      </div>
    {/if}
  </SharedVariablesLayout>
</div>
