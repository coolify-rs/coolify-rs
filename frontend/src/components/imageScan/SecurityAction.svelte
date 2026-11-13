<script lang="ts">
  // Port of upstream livewire/image-scan/partials/security-action.blade.php: one action of the
  // Security main card: its button (confirmation first) or its copy text. `withSentence` adds the
  // sentence for the other actions; the primary action's sentence is part of the image sentence.
  import ConfirmModal from '../ConfirmModal.svelte';
  import Reicon from '../Reicon.svelte';
  import type { CardAction } from '../../lib/imageScanApi';

  let {
    action,
    primary = false,
    withSentence = false,
    onsubmit,
  }: {
    action: CardAction;
    primary?: boolean;
    withSentence?: boolean;
    /** Runs the submit action of the confirmation (`rebuild`, `updateImage(…)`, `updateImages(…)`). */
    onsubmit: (submit: string) => Promise<void>;
  } = $props();

  let confirming = $state(false);
  let copied = $state(false);
  const code =
    'max-w-full truncate rounded-md border border-neutral-200 bg-neutral-50 px-2 py-1 font-mono text-[12px] text-neutral-800 dark:border-white/[0.08] dark:bg-white/[0.04] dark:text-fg';

  function copy(value: string) {
    navigator.clipboard?.writeText(value).catch(() => {});
    copied = true;
    setTimeout(() => (copied = false), 1000);
  }
</script>

<div class="flex min-w-0 flex-col gap-2 {withSentence ? 'sm:flex-row sm:items-center sm:justify-between sm:gap-4' : ''}">
  {#if withSentence}
    <p class="min-w-0 text-[13px] leading-5 text-neutral-800 dark:text-fg">{action.sentence}</p>
  {/if}

  {#if action.button}
    <div class="shrink-0">
      <button type="button" class="button normal-case! {primary ? 'button-highlighted' : ''}" onclick={() => (confirming = true)}>{action.button.label}</button>
    </div>
  {:else if action.copy}
    <div class="flex min-w-0 shrink-0 items-center gap-1">
      <code class={code} title={action.copy}>{action.copy}</code>
      <button type="button" title="Copy {action.copy}" aria-label="Copy {action.copy}" class="icon-button group shrink-0" onclick={() => copy(action.copy ?? '')}>
        <span class="inline-flex transition-transform duration-150 ease-out group-active:scale-75">
          {#if copied}<Reicon name="check" class="size-3.5 text-success" />{:else}<Reicon name="copy" class="size-3.5" />{/if}
        </span>
      </button>
    </div>
  {/if}
</div>

{#if confirming && action.button}
  <ConfirmModal
    title={action.button.title}
    actions={action.button.actions}
    warningMessage={action.button.warning}
    confirmWithText={false}
    confirmWithPassword={false}
    step2ButtonText={action.button.label}
    onconfirm={async () => {
      if (action.button) await onsubmit(action.button.submit);
    }}
    onclose={() => (confirming = false)}
  />
{/if}
