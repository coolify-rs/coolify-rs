<script lang="ts">
  // The Resources section of upstream livewire/source/github/resources.blade.php
  // and the end of livewire/source/{gitlab,gitea}/change.blade.php.
  import Empty from './Empty.svelte';
  import Reicon from './Reicon.svelte';
  import SettingsSection from './SettingsSection.svelte';
  import { link } from '../lib/router.svelte';
  import { strnatcmp } from '../lib/natsort';
  import type { SourceApplication } from '../lib/sourcesApi';

  let { provider, applications, repositoryColumn = false }: { provider: string; applications: SourceApplication[]; repositoryColumn?: boolean } = $props();

  let search = $state('');
  const sorted = $derived([...applications].sort((a, b) => strnatcmp(a.name, b.name)));
  const last = $derived((a: SourceApplication) => (repositoryColumn ? `${a.git_repository}:${a.git_branch}` : 'Application'));
  const shown = $derived(
    sorted.filter((a) => {
      const q = search.toLowerCase();
      return q === '' || `${a.project} ${a.environment} ${a.name} ${last(a)}`.toLowerCase().includes(q);
    }),
  );
  const cols = 'grid-cols-[minmax(10rem,.8fr)_minmax(10rem,.8fr)_minmax(12rem,1fr)_8rem]';
</script>

<SettingsSection title="Resources" description="Applications currently using this {provider} App." flush>
  {#if applications.length === 0}
    <Empty title="No resources use this source" description="Applications will appear here after this {provider} App is selected as their source." iconName="sources" size="sm" />
  {:else}
    <div class="border-b border-neutral-200 p-3 dark:border-white/[0.08]">
      <div class="relative w-full max-w-sm">
        <Reicon name="search" class="pointer-events-none absolute top-1/2 left-2.5 z-10 size-3.5 -translate-y-1/2 text-neutral-400 dark:text-fg-faint" />
        <input
          bind:value={search}
          type="search"
          placeholder="Search resources"
          class="h-8! w-full rounded-lg! border-neutral-200! bg-white! py-0! pr-3! pl-8! text-[12px]! shadow-none! placeholder:text-neutral-400 focus:border-accent! focus:ring-0! dark:border-white/[0.08]! dark:bg-white/[0.035]! dark:text-fg! dark:placeholder:text-fg-faint"
        />
      </div>
    </div>
    <div class="overflow-x-auto">
      <div class="grid min-w-[680px] {cols} border-b border-neutral-200 bg-neutral-50 px-4 py-2.5 text-[11px] font-medium text-neutral-500 dark:border-white/[0.08] dark:bg-white/[0.05] dark:text-fg-faint">
        <div>Project</div>
        <div>Environment</div>
        <div>Resource</div>
        <div>{repositoryColumn ? 'Repository' : 'Type'}</div>
      </div>
      {#each shown as a (a.uuid)}
        <a
          use:link
          href="/project/{a.project_uuid}/environment/{a.environment_uuid}/application/{a.uuid}"
          class="grid min-h-13 min-w-[680px] {cols} items-center border-b border-neutral-200 px-4 py-2.5 text-[12px] transition-colors last:border-b-0 hover:bg-neutral-50 hover:no-underline dark:border-white/[0.07] dark:hover:bg-white/[0.025]"
        >
          <span class="truncate text-neutral-500 dark:text-fg-dim">{a.project}</span>
          <span class="truncate text-neutral-500 dark:text-fg-dim">{a.environment}</span>
          <span class="truncate font-medium text-black dark:text-fg">{a.name}</span>
          <span class="{repositoryColumn ? 'truncate ' : ''}text-neutral-500 dark:text-fg-dim">{last(a)}</span>
        </a>
      {/each}
    </div>
  {/if}
</SettingsSection>
