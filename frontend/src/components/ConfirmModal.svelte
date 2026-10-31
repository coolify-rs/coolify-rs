<script lang="ts">
  import { untrack } from 'svelte';
  // Port of upstream components/modal-confirmation.blade.php: optional
  // checkbox step, the warning + type-the-name step, and the password step.
  // The server verifies the password; `onconfirm` returns an error message to
  // stay on the password step, or nothing when done.
  import Reicon from './Reicon.svelte';
  import Callout from './Callout.svelte';
  import Checkbox from './Checkbox.svelte';
  import { ApiError } from '../lib/api';
  import { notify } from '../lib/toast.svelte';
  import { session } from '../lib/session.svelte';

  interface CheckboxOption {
    id: string;
    label: string;
    defaultWarning?: string;
    /** Selected when the modal opens (the Livewire property's default). */
    checked?: boolean;
  }

  let {
    title = 'Are you sure?',
    actions = [],
    checkboxes = [],
    warningMessage = 'This operation is permanent and cannot be undone. Please think again before proceeding!',
    confirmWithText = true,
    confirmWithPassword = false,
    confirmationText = 'Confirm Deletion',
    confirmationLabel = 'Please confirm the execution of the actions by entering the Name below',
    shortConfirmationLabel = 'Name',
    step1ButtonText = 'Continue',
    step2ButtonText = 'Continue',
    step3ButtonText = 'Confirm',
    onconfirm,
    onclose,
  }: {
    title?: string;
    actions?: string[];
    checkboxes?: CheckboxOption[];
    warningMessage?: string;
    confirmWithText?: boolean;
    confirmWithPassword?: boolean;
    confirmationText?: string;
    confirmationLabel?: string;
    shortConfirmationLabel?: string;
    step1ButtonText?: string;
    step2ButtonText?: string;
    step3ButtonText?: string;
    onconfirm: (password: string, selected: string[]) => Promise<string | void>;
    onclose: () => void;
  } = $props();

  // svelte-ignore state_referenced_locally
  let step = $state(checkboxes.length ? 1 : 2);
  // The options' defaults, read once when the modal opens.
  let selected = $state<string[]>(untrack(() => checkboxes.filter((c) => c.checked).map((c) => c.id)));
  let typed = $state('');
  let password = $state('');
  let passwordError = $state('');
  let submitting = $state(false);
  let copied = $state(false);

  // Upstream only swaps "Continue" for "Confirm" when `shouldSkipPasswordConfirmation()` holds.
  const effectiveStep2 = $derived(session.info?.user.skip_password_confirmation && step2ButtonText === 'Continue' ? 'Confirm' : step2ButtonText);

  async function submit() {
    submitting = true;
    try {
      const error = await onconfirm(password, selected);
      if (typeof error === 'string') {
        passwordError = error;
        password = '';
        submitting = false;
        return;
      }
      onclose();
    } catch (e) {
      notify.error(e instanceof ApiError ? e.message : String(e), e instanceof ApiError ? e.details : undefined);
      submitting = false;
    }
  }

  function setAction(id: string, checked: boolean) {
    selected = selected.filter((s) => s !== id);
    if (checked) selected.push(id);
  }

  async function copy() {
    try {
      await navigator.clipboard.writeText(confirmationText);
      copied = true;
      setTimeout(() => (copied = false), 1500);
    } catch {
      // Clipboard access can be denied; the text is still selectable.
    }
  }
</script>

<svelte:window onkeydown={(e) => e.key === 'Escape' && onclose()} />

<div class="fixed inset-0 z-99 flex min-h-full items-center justify-center overflow-y-auto p-4">
  <div class="absolute inset-0 bg-black/50 backdrop-blur-[2px]" role="presentation" onclick={onclose}></div>
  <div
    role="dialog"
    aria-modal="true"
    aria-label={title}
    class="application-settings-form application-settings-section relative flex max-h-[calc(100dvh-2rem)] w-full flex-col lg:min-w-[36rem] lg:max-w-2xl"
    style="box-shadow: 0 0 0 1px var(--coollabs-hairline), var(--shadow-modal)"
  >
    <header class="flex-nowrap!">
      <h3 class="min-w-0 flex-1 truncate">{title}</h3>
      <button
        type="button"
        onclick={onclose}
        aria-label="Close"
        class="flex size-7 shrink-0 items-center justify-center rounded-md text-neutral-500 transition-colors hover:bg-neutral-100 hover:text-black dark:text-fg-faint dark:hover:bg-white/[0.06] dark:hover:text-fg"
      >
        <Reicon name="x" class="size-4" />
      </button>
    </header>
    <div class="application-settings-section-body min-h-0 flex-1 overflow-y-auto" style="-webkit-overflow-scrolling: touch;">
      {#if step === 1}
        <div>
          {#each checkboxes as c (c.id)}
            <div class="flex justify-between items-center mb-2">
              <Checkbox fullWidth id={c.id} label={c.label} checked={selected.includes(c.id)} onchange={(v) => setAction(c.id, v)} />
            </div>
          {/each}
          <div class="mt-4 flex flex-wrap justify-end gap-2 border-t border-neutral-200 pt-4 dark:border-white/[0.08]">
            <button type="button" class="button button-error w-auto" onclick={() => (step = 2)}>{step1ButtonText}</button>
          </div>
        </div>
      {:else if step === 2}
        <div>
          <Callout type="danger" title="Warning" class="mb-4">{@html warningMessage}</Callout>
          <div class="mb-2 text-[12px] font-medium text-neutral-700 dark:text-fg-dim">The following actions will be performed:</div>
          <ul class="mb-4 space-y-2">
            {#each actions as action, i (i)}
              <li class="flex items-start gap-2 text-[12px] leading-5 text-red-600 dark:text-red-400">
                <span class="shrink-0" aria-hidden="true">-</span>
                <span>{action}</span>
              </li>
            {/each}
            {#each checkboxes as c (c.id)}
              {#if selected.includes(c.id)}
                <li class="flex items-start gap-2 text-[12px] leading-5 text-red-600 dark:text-red-400">
                  <span class="shrink-0" aria-hidden="true">-</span>
                  <span>{c.label}</span>
                </li>
              {:else if c.defaultWarning}
                <li class="flex items-start gap-2 text-[12px] leading-5 text-red-600 dark:text-red-400">
                  <span class="shrink-0" aria-hidden="true">-</span>
                  <span>{c.defaultWarning}</span>
                </li>
              {/if}
            {/each}
          </ul>
          {#if confirmWithText}
            <div class="mb-4">
              <h4 class="mb-1 text-[12px] font-semibold">Confirm actions</h4>
              <p class="mb-2 text-[12px] leading-5 text-neutral-500 dark:text-fg-dim">{confirmationLabel}</p>
              <div class="relative mb-2">
                <div class="relative">
                  <input type="text" value={confirmationText} readonly class="input" />
                  <button
                    type="button"
                    title="Copy to clipboard"
                    aria-label="Copy to clipboard"
                    class="icon-button group shrink-0 absolute top-1/2 right-2 -translate-y-1/2"
                    onclick={copy}
                  >
                    <span class="inline-flex transition-transform duration-150 ease-out group-active:scale-75">
                      {#if copied}<Reicon name="check" class="size-3.5 text-success" />{:else}<Reicon name="copy" class="size-3.5" />{/if}
                    </span>
                  </button>
                </div>
              </div>
              <label for="userConfirmationText" class="mt-4 mb-1.5 block text-[12px] font-medium text-neutral-700 dark:text-fg-dim">
                {shortConfirmationLabel}
              </label>
              <input id="userConfirmationText" type="text" bind:value={typed} class="input w-full" />
            </div>
          {/if}
          <div class="mt-4 flex flex-wrap justify-end gap-2 border-t border-neutral-200 pt-4 dark:border-white/[0.08]">
            {#if checkboxes.length}
              <button type="button" class="button" onclick={() => (step = 1)}>Back</button>
            {:else}
              <button type="button" class="button" onclick={onclose}>Cancel</button>
            {/if}
            <button
              type="button"
              class="button button-error w-auto"
              disabled={submitting || (confirmWithText && typed !== confirmationText)}
              onclick={() => (confirmWithPassword ? (step = 3) : submit())}
            >
              {effectiveStep2}
            </button>
          </div>
        </div>
      {:else}
        <div>
          <Callout type="danger" title="Final Confirmation" class="mb-4">Please enter your password to confirm this destructive action.</Callout>
          <div class="flex flex-col gap-2 mb-4">
            <label for="password-confirm" class="block text-sm font-medium text-gray-700 dark:text-gray-300">Your Password</label>
            <!-- Enter must not submit the action (upstream @keydown.enter.prevent). -->
            <form onsubmit={(e) => e.preventDefault()}>
              <input type="text" name="username" autocomplete="username" value={session.info?.user.email ?? ''} style="display: none;" />
              <input
                type="password"
                id="password-confirm"
                bind:value={password}
                class="w-full input"
                placeholder="Enter your password"
                autocomplete="current-password"
                onkeydown={(e) => e.key === 'Enter' && e.preventDefault()}
              />
            </form>
            {#if passwordError}<p class="mt-1 text-sm text-red-500">{passwordError}</p>{/if}
          </div>
          <div class="mt-4 flex flex-wrap justify-end gap-2 border-t border-neutral-200 pt-4 dark:border-white/[0.08]">
            <button type="button" class="button" onclick={() => (step = 2)}>Back</button>
            <button type="button" class="button button-error w-auto" disabled={!password || submitting} onclick={submit}>{step3ButtonText}</button>
          </div>
        </div>
      {/if}
    </div>
  </div>
</div>
