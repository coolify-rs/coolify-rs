<script lang="ts">
  // Port of upstream livewire/shared-variables/project/index.blade.php,
  // livewire/shared-variables/environment/index.blade.php and
  // livewire/shared-variables/server/index.blade.php with
  // components/shared-variables/view-controls.blade.php.
  import Reicon from '../components/Reicon.svelte';
  import Empty from '../components/Empty.svelte';
  import SectionHeading from '../components/SectionHeading.svelte';
  import StatusBadge from '../components/StatusBadge.svelte';
  import SharedVariablesLayout from '../components/SharedVariablesLayout.svelte';
  import { sharedVariablesUi, type Owners } from '../lib/sharedVariablesApi';
  import { link } from '../lib/router.svelte';

  let { kind }: { kind: string } = $props();

  // svelte-ignore state_referenced_locally
  const label = kind === 'shared-projects' ? 'projects' : kind === 'shared-environments' ? 'environments' : 'servers';
  // svelte-ignore state_referenced_locally
  document.title = `${label === 'projects' ? 'Project' : label === 'environments' ? 'Environment' : 'Server'} Variables | coolify-rs`;
  const storageKey = `shared-variables-${label}-view`;

  let data = $state<Owners | null>(null);
  let search = $state('');
  let viewMode = $state(read());
  sharedVariablesUi.owners().then((d) => (data = d));

  function read(): string {
    try {
      return localStorage.getItem(`shared-variables-${kind.replace('shared-', '')}-view`) || 'grid';
    } catch {
      return 'grid';
    }
  }
  function setView(mode: string) {
    viewMode = mode;
    try {
      localStorage.setItem(storageKey, mode);
    } catch {
      // Storage unavailable: the choice lasts for this page only.
    }
  }
  function matches(values: (string | null | undefined)[]): boolean {
    const q = search.trim().toLowerCase();
    return !q || values.some((v) => String(v || '').toLowerCase().includes(q));
  }
  const card =
    'group flex flex-col rounded-xl border border-neutral-200 bg-white p-3 shadow-sm transition-all hover:-translate-y-px hover:border-neutral-300 hover:no-underline hover:shadow-md dark:border-white/[0.08] dark:bg-white/[0.05] dark:hover:border-white/[0.14]';
  const iconBox = 'flex size-8 shrink-0 items-center justify-center rounded-lg border border-neutral-200 bg-neutral-50 text-neutral-500 dark:border-white/[0.08] dark:bg-white/[0.04] dark:text-fg-dim';
  const rowCls = 'flex min-h-14 items-center gap-3 border-b border-neutral-200 px-4 py-2.5 last:border-b-0 hover:bg-neutral-50 hover:no-underline dark:border-white/[0.07] dark:hover:bg-white/[0.025]';
  const listBox = 'overflow-hidden rounded-xl border border-neutral-200 bg-white shadow-sm dark:border-white/[0.08] dark:bg-white/[0.05]';
  const variables = (n: number) => `${n} ${n === 1 ? 'variable' : 'variables'}`;
</script>

{#snippet controls()}
  <div class="mb-4 flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
    <div class="relative w-full sm:max-w-sm">
      <Reicon name="search" class="pointer-events-none absolute top-1/2 left-2.5 z-10 size-3.5 -translate-y-1/2 text-neutral-400 dark:text-fg-faint" />
      <input
        bind:value={search}
        type="search"
        placeholder="Search {label}"
        class="h-8! w-full rounded-lg! border-neutral-200! bg-white! py-0! pr-8! pl-8! text-[12px]! shadow-none! placeholder:text-neutral-400 focus:border-accent! focus:ring-0! dark:border-white/[0.08]! dark:bg-white/[0.035]! dark:text-fg! dark:placeholder:text-fg-faint"
      />
      {#if search}
        <button
          onclick={() => (search = '')}
          type="button"
          class="absolute top-1/2 right-2 flex size-5 -translate-y-1/2 items-center justify-center rounded text-neutral-400 transition-colors hover:bg-neutral-100 hover:text-black dark:text-fg-faint dark:hover:bg-white/[0.07] dark:hover:text-fg"
          aria-label="Clear search"
        >
          <Reicon name="x" class="size-3" />
        </button>
      {/if}
    </div>
    <div class="view-toggle w-fit">
      {#each [['list', 'List view', 'unordered-list'], ['grid', 'Grid view', 'grid']] as [mode, viewLabel, icon] (mode)}
        {#if viewMode !== mode}
          <button
            type="button"
            onclick={() => setView(mode)}
            title={viewLabel}
            aria-label={viewLabel}
            class="relative flex h-7.5 w-7.5 items-center justify-center rounded-md text-neutral-400 transition-colors hover:bg-neutral-100 hover:text-black dark:hover:text-white"
          >
            <Reicon name={icon} class="size-3.5" />
          </button>
        {/if}
      {/each}
    </div>
  </div>
{/snippet}

<div>
  <SharedVariablesLayout>
    {#if data}
      <div class="w-full">
        {#if kind === 'shared-projects'}
          {#if data.projects.length === 0}
            <Empty title="No projects yet" description="Create a project before adding project-wide variables." iconName="projects" />
          {:else}
            {@render controls()}
            {#if viewMode === 'grid'}
              <div class="grid grid-cols-1 gap-3 sm:grid-cols-2 lg:grid-cols-4">
                {#each data.projects as p (p.uuid)}
                  {#if matches([p.name, p.description])}
                    <a use:link class="{card} min-h-28" href="/shared-variables/project/{p.uuid}">
                      <div class="flex items-start gap-3">
                        <div class={iconBox}><Reicon name="projects" class="size-4" /></div>
                        <div class="min-w-0 flex-1 self-center">
                          <h2 class="truncate text-[13px]! leading-4! font-semibold! text-black dark:text-fg">{p.name}</h2>
                          {#if p.description}
                            <p class="mt-0.5 truncate text-[11px] text-neutral-500 dark:text-fg-faint">{p.description}</p>
                          {/if}
                        </div>
                      </div>
                      <div class="mt-auto pt-4 text-[11px] text-neutral-500 dark:text-fg-dim">{variables(p.variables)}</div>
                    </a>
                  {/if}
                {/each}
              </div>
            {:else}
              <div class={listBox}>
                {#each data.projects as p (p.uuid)}
                  {#if matches([p.name, p.description])}
                    <a use:link href="/shared-variables/project/{p.uuid}" class={rowCls}>
                      <Reicon name="projects" class="size-4 shrink-0 text-neutral-500 dark:text-fg-dim" />
                      <div class="min-w-0 flex-1">
                        <div class="truncate text-[13px] font-medium">{p.name}</div>
                        <div class="truncate text-[11px] text-neutral-500 dark:text-fg-faint">{p.description ?? ''}</div>
                      </div>
                      <span class="shrink-0 text-[11px] text-neutral-500 dark:text-fg-dim">{variables(p.variables)}</span>
                    </a>
                  {/if}
                {/each}
              </div>
            {/if}
          {/if}
        {:else if kind === 'shared-environments'}
          {#if data.projects.length === 0}
            <Empty title="No environments yet" description="Create a project environment before adding environment-wide variables." iconName="layers" />
          {:else}
            {@render controls()}
            {#if viewMode === 'grid'}
              <div class="flex flex-col gap-6">
                {#each data.projects as p (p.uuid)}
                  {#if matches([p.name, p.description, ...p.environments.flatMap((e) => [e.name, e.description])])}
                    <section>
                      <SectionHeading title={p.name} subtitle={p.description || 'Project environments'} />
                      {#if p.environments.length === 0}
                        <Empty title="No environments in this project." size="sm" />
                      {:else}
                        <div class="grid grid-cols-1 gap-3 sm:grid-cols-2 lg:grid-cols-4">
                          {#each p.environments as e (e.uuid)}
                            {#if matches([e.name, e.description, p.name])}
                              <a use:link class="{card} min-h-24" href="/shared-variables/environments/project/{p.uuid}/environment/{e.uuid}">
                                <div class="flex items-start gap-3">
                                  <div class={iconBox}><Reicon name="layers" class="size-4" /></div>
                                  <div class="min-w-0 flex-1 self-center">
                                    <h3 class="truncate text-[13px]! leading-4! font-semibold! text-black dark:text-fg">{e.name}</h3>
                                    {#if e.description}<p class="mt-0.5 truncate text-[11px] text-neutral-500 dark:text-fg-faint">{e.description}</p>{/if}
                                  </div>
                                </div>
                              </a>
                            {/if}
                          {/each}
                        </div>
                      {/if}
                    </section>
                  {/if}
                {/each}
              </div>
            {:else}
              <div class={listBox}>
                {#each data.projects as p (p.uuid)}
                  {#each p.environments as e (e.uuid)}
                    {#if matches([e.name, e.description, p.name])}
                      <a use:link href="/shared-variables/environments/project/{p.uuid}/environment/{e.uuid}" class={rowCls}>
                        <Reicon name="layers" class="size-4 shrink-0 text-neutral-500 dark:text-fg-dim" />
                        <div class="min-w-0 flex-1">
                          <div class="truncate text-[13px] font-medium">{e.name}</div>
                          <div class="truncate text-[11px] text-neutral-500 dark:text-fg-faint">{e.description ?? ''}</div>
                        </div>
                        <span class="shrink-0 text-[11px] text-neutral-500 dark:text-fg-dim">{p.name}</span>
                      </a>
                    {/if}
                  {/each}
                {/each}
              </div>
            {/if}
          {/if}
        {:else if data.servers.length === 0}
          <Empty title="No servers yet" description="Add a server before creating server-wide variables." iconName="servers" />
        {:else}
          {@render controls()}
          {#if viewMode === 'grid'}
            <div class="grid grid-cols-1 gap-3 sm:grid-cols-2 lg:grid-cols-4">
              {#each data.servers as s (s.uuid)}
                {#if matches([s.name, s.description, s.ip])}
                  <a use:link class="{card} min-h-28" href="/shared-variables/server/{s.uuid}">
                    <div class="flex items-start gap-3">
                      <div class={iconBox}><Reicon name="servers" class="size-4" /></div>
                      <div class="min-w-0 flex-1">
                        <h2 class="truncate text-[13px]! leading-4! font-semibold! text-black dark:text-fg">{s.name}</h2>
                        <p class="mt-0.5 truncate text-[11px] text-neutral-500 dark:text-fg-faint">{s.description || s.ip}</p>
                      </div>
                    </div>
                    <div class="mt-auto pt-4">
                      <StatusBadge label={s.functional ? 'Ready' : 'Validation required'} type={s.functional ? 'success' : 'warning'} />
                    </div>
                  </a>
                {/if}
              {/each}
            </div>
          {:else}
            <div class={listBox}>
              {#each data.servers as s (s.uuid)}
                {#if matches([s.name, s.description, s.ip])}
                  <a use:link href="/shared-variables/server/{s.uuid}" class={rowCls}>
                    <Reicon name="servers" class="size-4 shrink-0 text-neutral-500 dark:text-fg-dim" />
                    <div class="min-w-0 flex-1">
                      <div class="truncate text-[13px] font-medium">{s.name}</div>
                      <div class="truncate text-[11px] text-neutral-500 dark:text-fg-faint">{s.description || s.ip}</div>
                    </div>
                    <StatusBadge label={s.functional ? 'Ready' : 'Validation required'} type={s.functional ? 'success' : 'warning'} />
                  </a>
                {/if}
              {/each}
            </div>
          {/if}
        {/if}
      </div>
    {/if}
  </SharedVariablesLayout>
</div>
