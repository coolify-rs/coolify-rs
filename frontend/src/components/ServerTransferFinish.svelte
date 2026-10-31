<script lang="ts">
  // Port of upstream livewire/server/transfer-finish.blade.php.
  import ConfirmModal from './ConfirmModal.svelte';
  import Reicon from './Reicon.svelte';
  import { get, post, failure } from '../lib/serverApi';
  import { link } from '../lib/router.svelte';
  import { notify } from '../lib/toast.svelte';

  let { uuid }: { uuid: string } = $props();

  type Item = {
    key: string;
    type: string;
    label: string;
    status: string;
    hint: string | null;
    message: string | null;
    error: string | null;
    details: { label: string; value: string }[];
    group: string;
    can_check: boolean;
    fix_label: string | null;
    resolved_link: string | null;
    resolved_link_label: string | null;
  };
  type Finish = { last_checked_at: string | null; items: Item[] };

  const GROUPS: [string, string][] = [
    ['server', 'Server'],
    ['git', 'Git and webhooks'],
    ['dns', 'Domains and DNS'],
  ];
  const OPEN = ['pending', 'action', 'unchecked'];
  const BADGES: Record<string, [string, string]> = {
    action: ['Needs action', 'warning'],
    unchecked: ['Cannot check', 'neutral'],
    pending: ['Not checked yet', 'neutral'],
  };

  let finish = $state<Finish | null>(null);
  let canUpdate = $state(false);
  let openDetails = $state<Record<string, boolean>>({});
  let menu = $state('');
  let showDone = $state(false);
  let confirming = $state(false);

  const base = $derived(`/server-page/${encodeURIComponent(uuid)}/transfer-finish`);
  const items = $derived(finish?.items ?? []);
  const openItems = $derived(items.filter((i) => OPEN.includes(i.status)));
  const doneItems = $derived(items.filter((i) => !OPEN.includes(i.status)));
  const openCount = $derived(openItems.length);
  const totalCount = $derived(items.length);
  const doneCount = $derived(totalCount - openCount);
  const groups = $derived(
    GROUPS.map(([g, label]) => ({ label, items: openItems.filter((i) => i.group === g) })).filter((g) => g.items.length > 0),
  );

  async function refresh() {
    try {
      const r = await get<{ finish: Finish | null; can_update: boolean }>(base);
      finish = r.finish;
      canUpdate = r.can_update;
    } catch {
      finish = null;
    }
  }

  $effect(() => {
    refresh();
  });

  // `wire:poll.15s="refreshSteps"` while steps are open.
  $effect(() => {
    if (!finish || openCount === 0) return;
    const timer = setInterval(refresh, 15000);
    return () => clearInterval(timer);
  });

  async function call(path: string, body: unknown = {}) {
    try {
      const r = await post<Item & { message?: string }>(`${base}/${path}`, body);
      await refresh();
      return r;
    } catch (e) {
      failure(e);
      return null;
    }
  }

  async function fix(key: string) {
    const item = await call('fix', { key });
    if (!item) return;
    const message = item.message ?? 'Done.';
    if (item.status === 'ok') notify.success(message);
    else if (item.status === 'pending') notify.info(message);
    else notify.error(message);
  }

  async function finishTransfer(): Promise<string | void> {
    const r = await call('finish');
    confirming = false;
    if (r) notify.success('The transfer is finished.');
  }

  function clock(at: string): string {
    const d = new Date(at);
    return `${String(d.getUTCHours()).padStart(2, '0')}:${String(d.getUTCMinutes()).padStart(2, '0')}`;
  }
</script>

<div>
  {#if finish}
    <div class="mb-4 rounded-lg p-4 ring-1 ring-neutral-200 dark:ring-white/[0.08]" data-testid="transfer-finish">
      <div class="flex flex-wrap items-start justify-between gap-3">
        <div class="min-w-0">
          <h3 class="text-sm font-semibold text-neutral-950 dark:text-fg">Finish the transfer</h3>
          <p class="mt-1 text-xs text-neutral-500 dark:text-fg-dim">
            {doneCount} of {totalCount} done.
            {#if openCount > 0}
              Coolify checks the open steps every 30 seconds.
            {/if}
          </p>
        </div>
        <div class="flex items-center gap-2">
          {#if openCount > 0}
            <span class="text-xs text-neutral-500 dark:text-fg-dim">
              {finish.last_checked_at ? `Checked ${clock(finish.last_checked_at)}` : 'Checking…'}
            </span>
            {#if canUpdate}
              <button type="button" class="icon-button" title="Check now" onclick={() => call('check-all')}>
                <Reicon name="refresh" class="size-4" />
              </button>
            {/if}
          {:else}
            <button type="button" class="button" {...{ ishighlighted: "" }} disabled={!canUpdate} onclick={finishTransfer}>Finish the transfer</button>
          {/if}
        </div>
      </div>
      <div class="mt-3 h-1 overflow-hidden rounded-full bg-neutral-200 dark:bg-white/[0.08]">
        <div class="h-full rounded-full bg-success transition-all" style="width: {totalCount > 0 ? Math.round((doneCount / totalCount) * 100) : 0}%"></div>
      </div>

      {#each groups as group (group.label)}
        <div class="mt-4">
          <p class="text-xs font-medium text-neutral-500 dark:text-fg-dim">{group.label}</p>
          <ul class="mt-1 flex flex-col divide-y divide-neutral-200 dark:divide-white/[0.06]">
            {#each group.items as step (step.key)}
              {@const badge = BADGES[step.status] ?? ['Not checked yet', 'neutral']}
              {@const manualOnly = !step.can_check}
              {@const hasMenu = !manualOnly || step.resolved_link}
              {@const hasDetails = !!step.hint || step.details.length > 0 || !!step.error}
              <li class="py-2">
                <div class="flex items-center gap-3">
                  <span class="size-2 shrink-0 rounded-full {badge[1] === 'warning' ? 'bg-warning' : 'bg-neutral-400 dark:bg-neutral-500'}" title={badge[0]} role="img" aria-label={badge[0]}></span>
                  <div class="min-w-0 flex-1">
                    <p class="truncate text-sm text-neutral-950 dark:text-fg">{step.label}</p>
                    <p class="truncate text-xs text-neutral-500 dark:text-fg-dim">
                      {step.message ?? step.hint}
                      {#if hasDetails}
                        <button type="button" class="ml-1 underline" onclick={() => (openDetails[step.key] = !openDetails[step.key])}>{openDetails[step.key] ? 'Hide details' : 'Details'}</button>
                      {/if}
                    </p>
                  </div>
                  {#if canUpdate}
                    <div class="flex shrink-0 items-center gap-1">
                      {#if hasMenu}
                        <div class="relative">
                          <button type="button" class="icon-button" title="More actions" aria-haspopup="menu" aria-expanded={menu === step.key} onclick={() => (menu = menu === step.key ? '' : step.key)}>
                            <Reicon name="dots-horizontal" class="size-4" />
                          </button>
                          {#if menu === step.key}
                            <div role="menu" class="listbox-panel top-full! right-0! left-auto! z-[90]! mt-1! w-40! min-w-40!">
                              {#if !manualOnly}
                                <button type="button" class="listbox-option justify-start!" role="menuitem" onclick={() => ((menu = ''), call('check', { key: step.key }))}>Check now</button>
                              {/if}
                              <button type="button" class="listbox-option justify-start!" role="menuitem" onclick={() => ((menu = ''), call('done', { key: step.key }))}>Mark as done</button>
                              {#if step.fix_label && step.resolved_link}
                                <a href={step.resolved_link} use:link class="listbox-option justify-start!" role="menuitem">{step.resolved_link_label}</a>
                              {/if}
                            </div>
                          {/if}
                        </div>
                      {/if}
                      {#if step.fix_label}
                        <button type="button" class="button" onclick={() => fix(step.key)}>{step.fix_label}</button>
                      {:else if step.resolved_link}
                        <a href={step.resolved_link} use:link class="button">{step.resolved_link_label}</a>
                      {:else if manualOnly}
                        <button type="button" class="button" onclick={() => call('done', { key: step.key })}>Mark as done</button>
                      {/if}
                    </div>
                  {/if}
                </div>
                {#if hasDetails && openDetails[step.key]}
                  <div class="mt-2 ml-1 flex flex-col gap-1.5 text-xs text-neutral-600 dark:text-fg-dim">
                    {#if step.hint && step.message}
                      <p>{step.hint}</p>
                    {/if}
                    {#each step.details as detail (detail.label)}
                      <div class="flex min-w-0 items-center gap-2">
                        <span class="shrink-0">{detail.label}</span>
                        <code class="min-w-0 truncate rounded bg-neutral-100 px-1.5 py-0.5 dark:bg-white/[0.06]">{detail.value}</code>
                        <button type="button" class="icon-button" title="Copy" onclick={() => navigator.clipboard?.writeText(detail.value)}><Reicon name="copy" class="size-3.5" /></button>
                      </div>
                    {/each}
                    {#if step.error}
                      <p class="break-words">{step.error}</p>
                    {/if}
                  </div>
                {/if}
              </li>
            {/each}
          </ul>
        </div>
      {/each}

      {#if doneCount > 0}
        <div class="mt-4">
          <button type="button" class="flex items-center gap-1 text-xs text-neutral-500 dark:text-fg-dim" onclick={() => (showDone = !showDone)}>
            <Reicon name="chevron-down" class="size-3 transition-transform {showDone ? '' : '-rotate-90'}" />
            {doneCount} {doneCount === 1 ? 'step is' : 'steps are'} OK or done
          </button>
          {#if showDone}
            <ul class="mt-1 flex flex-col gap-1">
              {#each doneItems as step (step.key)}
                <li class="flex items-center gap-2 text-xs text-neutral-500 dark:text-fg-dim">
                  <Reicon name="check-circle" class="size-3.5 text-success" />
                  <span class="text-neutral-700 dark:text-fg">{step.label}</span>
                  <span class="truncate">{step.message}</span>
                </li>
              {/each}
            </ul>
          {/if}
        </div>
      {/if}

      {#if openCount > 0 && canUpdate}
        <div class="mt-4 flex justify-end">
          <button type="button" class="text-xs text-neutral-500 underline dark:text-fg-dim" onclick={() => (confirming = true)}>Finish with open steps…</button>
        </div>
      {/if}
    </div>
  {/if}
</div>

{#if confirming}
  <ConfirmModal
    title="Finish with open steps?"
    confirmWithText={false}
    confirmWithPassword={false}
    step2ButtonText="Finish the transfer"
    warningMessage={`<strong>${openCount} steps are not OK or done.</strong> Coolify does not check them again. Webhooks, DNS records, or runners can still point to the other instance.`}
    actions={groups.flatMap((g) => g.items).map((s) => `Skipped: ${s.label}`)}
    onconfirm={finishTransfer}
    onclose={() => (confirming = false)}
  />
{/if}
