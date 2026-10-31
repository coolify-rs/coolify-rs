<script lang="ts">
  // Port of upstream livewire/upgrade.blade.php (App\Livewire\Upgrade): the badge or full button and
  // the confirmation view. The progress view is not ported, because the upgrade itself is not.
  import Callout from './Callout.svelte';
  import Reicon from './Reicon.svelte';
  import { ApiError } from '../lib/api';
  import { settingsUi, type UpgradeState } from '../lib/settingsApi';
  import { notify } from '../lib/toast.svelte';

  let { fullButton = false, state: given = null }: { fullButton?: boolean; state?: UpgradeState | null } = $props();

  let fetched = $state<UpgradeState | null>(null);
  let open = $state(false);
  const upgrade = $derived(given ?? fetched);

  $effect(() => {
    if (!given) settingsUi.upgradeState().then((s) => (fetched = s)).catch(() => {});
  });

  async function confirmed() {
    try {
      const r = await settingsUi.upgrade();
      if (r.message) notify.success(r.message);
    } catch (e) {
      notify.failure(e instanceof ApiError ? e.message : String(e));
    }
    open = false;
  }
</script>

<svelte:window onkeydown={(e) => e.key === 'Escape' && (open = false)} />

<div title={upgrade?.available ? 'New version available' : 'No upgrade available'}>
  {#if upgrade?.available}
    <div class="relative w-auto h-auto {open ? 'z-40' : ''}">
      {#if fullButton}
        <button type="button" class="button button-highlighted" onclick={() => (open = true)}>Upgrade now</button>
      {:else}
        <button
          type="button"
          title="Update available"
          aria-label="Update available"
          onclick={() => (open = true)}
          class="inline-flex h-[18px] cursor-pointer items-center rounded-full bg-coollabs/10 px-1.5 text-[9.5px] font-semibold leading-none text-coollabs ring-1 ring-inset ring-coollabs/25 transition-colors hover:bg-coollabs/15 dark:bg-warning/15 dark:text-warning dark:ring-warning/25 dark:hover:bg-warning/20"
        >
          Update available
        </button>
      {/if}
      {#if open}
        <div class="fixed inset-0 z-99 flex min-h-full items-center justify-center overflow-y-auto p-4">
          <div class="absolute inset-0 w-full h-full bg-black/50 backdrop-blur-[2px]"></div>
          <div
            class="application-settings-form application-settings-section relative flex max-h-[calc(100dvh-2rem)] w-full flex-col lg:min-w-[36rem] lg:max-w-2xl"
            style="box-shadow: 0 0 0 1px var(--coollabs-hairline), var(--shadow-modal)"
          >
            <header class="flex-nowrap!">
              <div class="min-w-0 flex-1">
                <h3 class="truncate">Upgrade available</h3>
                <p class="mt-0.5 text-[12px] leading-4" style="color: var(--coollabs-subtle)">
                  {upgrade.current_version} <span class="mx-0.5">&rarr;</span>
                  {upgrade.latest_version}
                </p>
              </div>
              <button
                type="button"
                onclick={() => (open = false)}
                class="flex size-7 shrink-0 cursor-pointer items-center justify-center rounded-md text-neutral-500 outline-0 transition-colors hover:bg-neutral-100 hover:text-black focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-accent dark:text-fg-faint dark:hover:bg-white/[0.06] dark:hover:text-fg"
                aria-label="Close"
              >
                <Reicon name="x" class="size-4" />
              </button>
            </header>
            <div class="application-settings-section-body min-h-0 flex-1 overflow-y-auto" style="-webkit-overflow-scrolling: touch;">
              <div class="flex flex-col gap-4">
                <Callout type="warning" title="Caution">
                  <p>Any deployments running during the update process will fail.</p>
                </Callout>
                <p class="text-[12px] leading-5" style="color: var(--coollabs-subtle)">
                  If something goes wrong, check the
                  <a
                    class="font-medium text-coollabs underline decoration-coollabs/30 underline-offset-2 hover:decoration-coollabs dark:text-brand-light dark:decoration-brand-light/30 dark:hover:decoration-brand-light"
                    href="https://coolify.io/docs/upgrade"
                    target="_blank"
                    rel="noopener noreferrer">upgrade guide</a
                  >
                  or the logs on the server at
                  <span class="font-mono text-neutral-700 dark:text-fg">/data/coolify/source/upgrade*</span>.
                </p>
                <div class="flex flex-wrap items-center justify-end gap-2 border-t border-neutral-200 pt-4 dark:border-white/[0.08]">
                  <button type="button" class="button button-highlighted" onclick={confirmed}>Upgrade now</button>
                </div>
              </div>
            </div>
          </div>
        </div>
      {/if}
    </div>
  {:else if fullButton}
    <p class="text-sm text-neutral-600 dark:text-fg-dim">Coolify is up to date.</p>
  {/if}
</div>
