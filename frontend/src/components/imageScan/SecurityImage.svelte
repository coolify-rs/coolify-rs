<script lang="ts">
  // Port of upstream livewire/image-scan/partials/security-image.blade.php: the open content of
  // one image of the Security main card (spec 15.11): the sentence, the primary action (one image
  // only; a list shows it in the row header), the attack note, the why-list, the provenance, and
  // the other fixes.
  import { link } from '../../lib/router.svelte';
  import Reicon from '../Reicon.svelte';
  import SecurityAction from './SecurityAction.svelte';
  import SeverityBadge from './SeverityBadge.svelte';
  import type { ImageRow } from '../../lib/imageScanApi';

  let {
    row,
    single,
    canUpdate,
    onsubmit,
    onshowfindings,
    onremoveoverride,
  }: {
    row: ImageRow;
    single: boolean;
    canUpdate: boolean;
    onsubmit: (submit: string) => Promise<void>;
    /** The "{n} more problems" link: the findings of this image, then the findings section. */
    onshowfindings: (filter: string | null) => void;
    onremoveoverride: (service: string) => void;
  } = $props();

  const muted = 'text-[11px] text-neutral-500 dark:text-fg-faint';
  const note = 'text-[13px] leading-5 text-neutral-600 dark:text-fg-dim';
  const linkClass =
    'font-medium text-neutral-800 underline decoration-neutral-400/40 underline-offset-4 transition-colors hover:text-neutral-950 hover:decoration-current dark:text-fg dark:decoration-white/25 dark:hover:text-white dark:hover:decoration-current';
  const motion = 'transition-transform duration-[180ms] ease-[cubic-bezier(0.23,1,0.32,1)] motion-reduce:transition-none';
  const startsWithRef = $derived(row.ref.length > 0 && row.sentence.startsWith(row.ref));
  const whyTitle = $derived(row.status === 'no_fix' ? 'Not fixed yet' : 'Fixed by this update');
  const otherCount = $derived(row.others.length + row.moreActions);
</script>

<div class="flex flex-col gap-4">
  <div class="flex max-w-3xl flex-col gap-3">
    <p class="text-pretty text-[15px] font-medium leading-6 text-neutral-950 dark:text-fg">
      {#if startsWithRef}<code class="break-all font-mono text-[14px]">{row.ref}</code>{row.sentence.slice(row.ref.length)}{:else}{row.sentence}{/if}
    </p>

    {#if single && row.status !== 'check_needed' && row.primary}
      <SecurityAction action={row.primary} primary {onsubmit} />
    {/if}

    {#if row.attackNote !== null || row.advice !== null || row.lines.length > 0 || (single && row.versionNotes.length > 0)}
      <div class="flex flex-col gap-1">
        {#if row.attackNote !== null}
          <p class={note}>{row.attackNote}</p>
        {/if}
        {#if row.advice !== null}
          <p class={note}>{row.advice}</p>
        {/if}
        {#each row.lines as line, i (i)}
          <p class={note}>{line}</p>
        {/each}
        <!-- In a list, the row header shows these notes. -->
        {#each single ? row.versionNotes : [] as versionNote, i (i)}
          <p class={note}>
            {versionNote.text}
            {#if versionNote.link !== null}
              <a href={versionNote.link} use:link class={linkClass}>Registry logins</a>
            {/if}
          </p>
        {/each}
      </div>
    {/if}
  </div>

  {#if row.override !== null}
    <!-- A security update of a Compose app: Coolify replaces the image of the compose file from Git. -->
    <p class="max-w-3xl text-[12px] leading-5 text-neutral-500 dark:text-fg-faint">
      Coolify deploys <code class="font-mono">{row.override.to}</code> instead of
      <code class="font-mono">{row.override.from}</code> from your compose file.
      {#if canUpdate}
        <button
          type="button"
          class={linkClass}
          onclick={() => row.override && onremoveoverride(row.override.service)}
          title="The next deploy uses the image of the compose file again.">Use the compose file</button
        >
      {/if}
    </p>
  {/if}

  {#if row.why.length > 0}
    <div class="flex flex-col gap-2">
      <h4 class="text-[12px] font-medium text-neutral-700 dark:text-fg-dim">{whyTitle}</h4>
      <ul
        class="flex flex-col divide-y divide-neutral-200 overflow-hidden rounded-lg border border-neutral-200 dark:divide-white/[0.06] dark:border-white/[0.08]"
        aria-label={whyTitle}
      >
        {#each row.why as entry, i (`${entry.display_id}-${entry.title}-${i}`)}
          <li class="flex flex-col gap-1 px-3 py-2.5">
            <div class="flex min-w-0 flex-col gap-1.5 sm:flex-row sm:items-start sm:justify-between sm:gap-4">
              <span class="min-w-0 text-[13px] leading-5 text-neutral-900 dark:text-fg">{entry.title}</span>
              <span class="flex shrink-0 items-center gap-1.5">
                {#if entry.cvss !== null}
                  <SeverityBadge severity={entry.severity} />
                  <span class="text-[12px] tabular-nums text-neutral-600 dark:text-fg-dim">{entry.cvss}</span>
                {:else if entry.cvssNote !== null}
                  <span data-tooltip={entry.cvssNote} title={entry.cvssNote}>
                    <SeverityBadge severity={entry.severity} />
                  </span>
                {:else}
                  <SeverityBadge severity={entry.severity} />
                {/if}
              </span>
            </div>
            <!-- Each separator stays with the part after it, so a wrapped line never ends with "·". -->
            <div class="flex min-w-0 flex-wrap items-center gap-x-1.5 {muted}">
              {#if entry.package !== ''}
                <span class="min-w-0 break-all font-mono max-sm:w-full">{`${entry.package} ${entry.version}`.trim()}</span>
              {/if}
              <span class="inline-flex items-center gap-1.5 whitespace-nowrap">
                {#if entry.package !== ''}
                  <span class="max-sm:hidden" aria-hidden="true">·</span>
                {/if}
                {#if entry.link !== null}
                  <a
                    href={entry.link}
                    target="_blank"
                    rel="noopener noreferrer"
                    class="font-mono underline decoration-neutral-400/40 underline-offset-4 hover:text-neutral-950 hover:decoration-current dark:decoration-white/25 dark:hover:text-fg"
                    >{entry.display_id}</a
                  >
                {:else}
                  <span class="font-mono">{entry.display_id}</span>
                {/if}
              </span>
              {#if entry.signal !== null}
                <span class="inline-flex items-center gap-1.5 whitespace-nowrap">
                  <span aria-hidden="true">·</span>
                  <span class={entry.exploited ? 'text-error' : ''}>{entry.signal}</span>
                </span>
              {/if}
            </div>
          </li>
        {/each}
      </ul>
      {#if row.whyMore > 0}
        <div>
          <button type="button" class="text-[12px] {linkClass}" onclick={() => onshowfindings(row.filter)}>
            {row.whyMore.toLocaleString('en-US')} more {row.whyMore === 1 ? 'problem' : 'problems'} this update fixes <span aria-hidden="true">→</span>
          </button>
        </div>
      {/if}
    </div>
  {/if}

  {#if row.provenance !== null}
    <p class="max-w-3xl text-[12px] leading-5 text-neutral-500 dark:text-fg-faint">
      <span class="font-medium text-neutral-700 dark:text-fg-dim">How Coolify knows:</span>
      {row.provenance}
    </p>
  {/if}

  {#if row.others.length > 0}
    <details class="group/others">
      <summary
        class="inline-flex cursor-pointer list-none items-center gap-1.5 rounded-md text-[12px] font-medium text-neutral-600 hover:text-neutral-950 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-coollabs dark:text-fg-dim dark:hover:text-fg dark:focus-visible:outline-brand [&::-webkit-details-marker]:hidden"
      >
        <Reicon name="chevron-down" class="size-3.5 shrink-0 -rotate-90 group-open/others:rotate-0 {motion}" />
        {otherCount.toLocaleString('en-US')} other {otherCount === 1 ? 'fix' : 'fixes'}
      </summary>
      <div class="mt-2 flex flex-col divide-y divide-neutral-200 rounded-lg border border-neutral-200 dark:divide-white/[0.06] dark:border-white/[0.08]">
        {#each row.others as other (other.key)}
          <div class="px-3 py-2.5">
            <SecurityAction action={other} withSentence {onsubmit} />
          </div>
        {/each}
      </div>
      {#if row.moreActions > 0}
        <p class="mt-2 {muted} text-[12px]!">and {row.moreActions.toLocaleString('en-US')} more</p>
      {/if}
    </details>
  {/if}
</div>
