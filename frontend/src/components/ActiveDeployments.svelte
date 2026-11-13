<script lang="ts">
  // Port of upstream livewire/dashboard/active-deployments.blade.php
  // (App\Livewire\Dashboard\ActiveDeployments).
  import SectionHeading from './SectionHeading.svelte';
  import StatusBadge from './StatusBadge.svelte';
  import { dayDateTime, diffForHumans, iso8601 } from '../lib/format';
  import { layoutUi, type QueueRow } from '../lib/layoutApi';
  import { link } from '../lib/router.svelte';

  let active = $state<QueueRow[]>([]);
  let recent = $state<QueueRow[]>([]);

  const any = $derived(active.length > 0 || recent.length > 0);

  $effect(() => {
    const load = () =>
      layoutUi.recent().then((d) => {
        active = d.active;
        recent = d.recent;
      }).catch(() => {});
    load();
    const id = setInterval(load, 3000);
    return () => clearInterval(id);
  });

  function meta(d: QueueRow): [string, 'success' | 'warning' | 'error' | 'neutral'] {
    const status = d.status;
    // `$deployment->isImageScanBlocked()`: the status stays failed, the badge says "Blocked".
    if (d.image_scan_blocked) return ['Blocked', 'error'];
    const label =
      { finished: 'Success', in_progress: 'In progress', 'cancelled-by-user': 'Cancelled', queued: 'Queued', failed: 'Failed' }[status] ??
      status.replace(/[-_]/g, ' ').replace(/\b\w/g, (c) => c.toUpperCase());
    const type = status === 'finished' ? 'success' : status === 'in_progress' || status === 'queued' ? 'warning' : status === 'failed' ? 'error' : 'neutral';
    return [label, type];
  }
  const rowClass =
    'dashboard-deployment-table-grid group block border-b border-neutral-200 px-4 py-3 transition-colors last:border-b-0 hover:bg-neutral-50 focus-visible:z-10 focus-visible:bg-coollabs/10 focus-visible:ring-1 focus-visible:ring-inset focus-visible:ring-coollabs focus-visible:ring-offset-0 @2xl:grid @2xl:items-center @2xl:gap-4 dark:border-white/[0.08] dark:hover:bg-white/[0.025] dark:focus-visible:bg-brand/10 dark:focus-visible:ring-brand-light';
  const headClass =
    'dashboard-deployment-table-grid hidden items-center gap-4 border-b border-neutral-200 bg-neutral-50 px-4 py-2.5 text-[11px] font-medium text-neutral-500 @2xl:grid dark:border-white/[0.08] dark:bg-white/[0.05] dark:text-fg-faint';
  const boxClass = 'overflow-hidden rounded-xl border border-neutral-200 bg-white shadow-sm dark:border-white/[0.08] dark:bg-white/[0.05]';
</script>

{#snippet head(time: string)}
  <div class={headClass}>
    <span>Application</span><span>Environment</span><span>Server</span><span>Status</span><span>{time}</span>
  </div>
{/snippet}

<!-- Active deployments show when they started, recent ones when they finished. -->
{#snippet row(d: QueueRow, finished: boolean)}
  {@const st = meta(d)}
  {@const path = [d.project, d.environment].filter(Boolean).join(' / ')}
  {@const at = finished ? d.finished_at : d.created_at}
  <a href={d.deployment_url} use:link class={rowClass}>
    <div class="min-w-0">
      <p class="truncate text-[13px] font-semibold text-black dark:text-fg">{d.application_name}</p>
      {#if d.pull_request_id}
        <p class="mt-0.5 text-[11px] text-neutral-500 dark:text-fg-faint">Pull request #{d.pull_request_id}</p>
      {/if}
    </div>
    <p class="hidden truncate text-[12px] text-neutral-500 @2xl:block dark:text-fg-dim">{path || '-'}</p>
    <p class="hidden truncate text-[12px] text-neutral-500 @2xl:block dark:text-fg-dim">{d.server_name || '-'}</p>
    <span class="hidden @2xl:block"><StatusBadge label={st[0]} type={st[1]} spinning={d.status === 'in_progress'} /></span>
    <p class="hidden truncate text-[12px] text-neutral-500 @2xl:block dark:text-fg-dim" title={finished ? dayDateTime(at) : undefined}>
      {diffForHumans(at) || '-'}
    </p>
    <div class="mt-3 flex min-w-0 items-center gap-3 @2xl:hidden">
      <StatusBadge label={st[0]} type={st[1]} spinning={d.status === 'in_progress'} />
      <p class="min-w-0 truncate text-[11px] text-neutral-500 dark:text-fg-faint">
        {path || '-'}
      </p>
      {#if at}
        <time datetime={iso8601(at)} title={dayDateTime(at)} class="ml-auto shrink-0 whitespace-nowrap text-[11px] text-neutral-500 dark:text-fg-faint">
          {diffForHumans(at)}
        </time>
      {/if}
    </div>
  </a>
{/snippet}

<div class="min-w-0 {any ? 'mb-0!' : 'hidden'}">
  {#if any}
    <section class="@container mb-0! min-w-0">
      <SectionHeading title={recent.length > 0 ? 'Recent deployments' : 'Deployments'} class="py-1.5" />
      <div class="flex min-w-0 flex-col gap-4">
        {#if active.length > 0}
          <div class={boxClass}>
            <div class="border-b border-neutral-200 px-4 py-2.5 dark:border-white/[0.08]">
              <h3 class="text-[11px]! leading-4! font-medium! text-neutral-500! dark:text-fg-faint!">Active</h3>
              <p class="mt-0.5 text-[11px] text-neutral-500 dark:text-fg-faint">Running or queued right now</p>
            </div>
            {@render head('Started')}
            {#each active as d (d.deployment_uuid)}{@render row(d, false)}{/each}
          </div>
        {/if}
        {#if recent.length > 0}
          <div class={boxClass}>
            {@render head('Finished')}
            {#each recent as d (d.deployment_uuid)}{@render row(d, true)}{/each}
          </div>
        {/if}
      </div>
    </section>
  {/if}
</div>
