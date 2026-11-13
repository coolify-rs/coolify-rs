<script lang="ts">
  // Port of upstream livewire/image-scan/partials/finding-row.blade.php: one finding of the
  // findings table, with its details. A nested row (inside a package group) shows its details
  // at once.
  import Reicon from '../Reicon.svelte';
  import StatusBadge from '../StatusBadge.svelte';
  import SeverityBadge from './SeverityBadge.svelte';
  import type { FindingRow } from '../../lib/imageScanApi';

  let {
    row,
    nested = false,
    showsImage = false,
    canUpdate = false,
    onignore,
  }: {
    row: FindingRow;
    nested?: boolean;
    showsImage?: boolean;
    canUpdate?: boolean;
    onignore: (detail: { target: string; kind: string; owner: string; neverAllowed: boolean }) => void;
  } = $props();

  let open = $state(false);
  const muted = 'text-[11px] text-neutral-500 dark:text-fg-faint';
  const hasDetails = $derived(row.summary !== '' || row.likelihood !== null || row.aliases.length > 0 || row.ignore !== null || !!row.base);
</script>

<div class="border-b border-neutral-200 last:border-b-0 dark:border-white/[0.06]">
  <div class="data-table-row image-scan-findings-grid {nested ? 'is-nested' : ''} {!nested && showsImage ? 'has-image' : ''}">
    <div class="flex min-w-0 items-center gap-1.5">
      {#if !nested}
        <button type="button" class="icon-button" onclick={() => (open = !open)} aria-expanded={open ? 'true' : 'false'} aria-label="Show details of {row.id}">
          <Reicon name="chevron-down" class="size-3.5 transition-transform duration-150 {open ? 'rotate-0' : '-rotate-90'}" />
        </button>
      {/if}
      {#if row.url !== null}
        <a
          href={row.url}
          target="_blank"
          rel="noopener noreferrer"
          class="truncate font-mono text-[12px] font-medium text-neutral-950 underline-offset-4 hover:underline dark:text-fg">{row.id}</a
        >
      {:else}
        <span class="truncate font-mono text-[12px] font-medium text-neutral-950 dark:text-fg">{row.id}</span>
      {/if}
    </div>
    {#if !nested}
      <span class="image-scan-secondary min-w-0 truncate text-[12px] text-neutral-700 dark:text-fg-dim" title={row.package}>{row.package}</span>
      {#if showsImage}
        <span class="image-scan-secondary min-w-0 truncate {muted}" title={row.image}>{row.image}</span>
      {/if}
    {/if}
    <span class="image-scan-secondary min-w-0 truncate font-mono text-[11px] text-neutral-600 dark:text-fg-dim">
      {row.version}{#if row.fixed_in}{` → ${row.fixed_in}`}{/if}
    </span>
    <span class="image-scan-severity-cell flex min-w-0 flex-wrap items-center gap-1">
      <SeverityBadge severity={row.severity} />
      {#if row.kev}
        <span data-tooltip="Attackers are using this, according to CISA.">
          <StatusBadge type="error" label="Known exploited" />
        </span>
      {/if}
    </span>
    <span class="flex justify-end">
      {#if row.ignore !== null}
        <span class={muted} data-tooltip={row.ignore.reason}>
          Ignored{row.ignore.until ? ` until ${row.ignore.until}` : ''}
        </span>
      {:else if canUpdate}
        <button
          type="button"
          class="icon-button"
          data-tooltip="Ignore"
          aria-label="Ignore {row.id}"
          onclick={() => onignore({ target: row.id, kind: 'vulnerability', owner: row.owner, neverAllowed: row.never_allowed })}
        >
          <Reicon name="eye-off2" class="size-3.5" />
        </button>
      {/if}
    </span>
  </div>
  {#if hasDetails && (nested || open)}
    <div class="flex flex-col gap-1 px-4 pb-3 text-[12px] text-neutral-600 dark:text-fg-dim {!nested ? 'pl-12' : ''}">
      {#if row.summary !== ''}
        <p class="max-w-3xl">{row.summary}</p>
      {/if}
      {#if row.likelihood !== null}
        <p class={muted}>Exploit likelihood: {row.likelihood}</p>
      {/if}
      {#if row.base}
        <p class={muted}>Comes from <span class="font-mono">{row.base}</span>.</p>
      {/if}
      {#if row.aliases.length > 0}
        <p class={muted}>
          Also known as
          {#each row.aliases as alias, i (alias.id)}
            {#if alias.url !== null}
              <a href={alias.url} target="_blank" rel="noopener noreferrer" class="font-mono underline-offset-4 hover:underline">{alias.id}</a>{i === row.aliases.length - 1 ? '' : ','}
            {:else}
              <span class="font-mono">{alias.id}</span>{i === row.aliases.length - 1 ? '' : ','}
            {/if}
            {' '}
          {/each}
        </p>
      {/if}
      {#if row.ignore !== null}
        <p class={muted}>Reason: {row.ignore.reason}</p>
      {/if}
    </div>
  {/if}
</div>
