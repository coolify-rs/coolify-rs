<script lang="ts">
  // Port of upstream livewire/upgrade.blade.php (App\Livewire\Upgrade) and components/upgrade-progress.blade.php.
  import Callout from './Callout.svelte';
  import Reicon from './Reicon.svelte';
  import { ApiError } from '../lib/api';
  import { settingsUi, type UpgradeState } from '../lib/settingsApi';
  import { notify } from '../lib/toast.svelte';
  import { portal } from '../lib/portal';

  let { fullButton = false, state: given = null }: { fullButton?: boolean; state?: UpgradeState | null } = $props();

  let fetched = $state<UpgradeState | null>(null);
  let open = $state(false);
  const upgrade = $derived(given ?? fetched);

  $effect(() => {
    if (!given) settingsUi.upgradeState().then((s) => (fetched = s)).catch(() => {});
  });

  const STEPS = ['Preparing', 'Helper', 'Image', 'Restart'];
  let showProgress = $state(false);
  let currentStep = $state(0);
  let currentStatus = $state('');
  let elapsedTime = $state(0);
  let upgradeComplete = $state(false);
  let upgradeError = $state(false);
  let successCountdown = $state(3);
  let startTime = 0;
  let elapsedTimer: ReturnType<typeof setInterval> | null = null;
  let statusTimer: ReturnType<typeof setInterval> | null = null;
  let healthTimer: ReturnType<typeof setInterval> | null = null;
  let statusPending = false;
  let healthPending = false;
  let attempts = 0;
  let failures = 0;
  let healthTimeouts = 0;
  let wentDown = false;
  const latestVersion = $derived(upgrade?.latest_version ?? '');

  const elapsedText = $derived(`${Math.floor(elapsedTime / 60)}:${String(elapsedTime % 60).padStart(2, '0')}`);
  const beforeUnload = (event: BeforeUnloadEvent) => {
    event.preventDefault();
    event.returnValue = '';
  };

  function stopTimers() {
    for (const t of [elapsedTimer, statusTimer, healthTimer]) if (t) clearInterval(t);
    elapsedTimer = statusTimer = healthTimer = null;
    window.removeEventListener('beforeunload', beforeUnload);
  }

  function reviveMessage(minutes: number) {
    if (minutes === 0) return `Waiting for Coolify to come back online... (attempt ${attempts})`;
    if (minutes < 2) return `Waiting for Coolify to come back online... (${minutes} minute${minutes > 1 ? 's' : ''} elapsed)`;
    if (minutes < 5) return `Update in progress, this may take several minutes... (${minutes} minutes elapsed)`;
    if (minutes < 10) return `Large updates can take 10+ minutes. Please be patient... (${minutes} minutes elapsed)`;
    return `Still updating. If this takes longer than 15 minutes, please check server logs... (${minutes} minutes elapsed)`;
  }

  function showSuccess() {
    if (upgradeComplete) return;
    stopTimers();
    upgradeComplete = true;
    currentStep = 5;
    currentStatus = `Successfully upgraded to ${latestVersion}`;
    successCountdown = 3;
    const countdown = setInterval(() => {
      successCountdown--;
      if (successCountdown <= 0) {
        clearInterval(countdown);
        window.location.reload();
      }
    }, 1000);
  }

  function showError(message?: string) {
    stopTimers();
    upgradeError = true;
    currentStatus = `Error: ${message}`;
  }

  function closeErrorModal() {
    open = false;
    showProgress = false;
    upgradeError = false;
    currentStatus = '';
    currentStep = 0;
  }

  async function probeHealth() {
    if (healthPending || upgradeComplete || upgradeError) return;
    healthPending = true;
    attempts++;
    const minutes = Math.floor((Date.now() - startTime) / 60000);
    try {
      const response = await fetch('/api/health', { cache: 'no-store', signal: AbortSignal.timeout(10000) });
      healthTimeouts = 0;
      if (!response.ok) {
        wentDown = true;
        currentStep = 4;
        currentStatus = reviveMessage(minutes);
        return;
      }
      if (!wentDown && currentStep < 4) return;
      let data;
      try {
        data = await settingsUi.upgradeStatus();
      } catch (error) {
        if (wentDown) {
          showSuccess();
          return;
        }
        throw error;
      }
      if (data.status === 'complete') showSuccess();
      else if (data.status === 'error') showError(data.message);
      else if (data.status === 'none' && wentDown) showSuccess();
      else currentStatus = data.message ?? reviveMessage(minutes);
    } catch (error) {
      if ((error as Error)?.name === 'TimeoutError' && ++healthTimeouts < 2) return;
      wentDown = true;
      currentStep = 4;
      currentStatus = reviveMessage(minutes);
    } finally {
      healthPending = false;
    }
  }

  function watchStatus() {
    currentStep = 1;
    currentStatus = 'Starting upgrade...';
    healthTimer = setInterval(probeHealth, 2000);
    statusTimer = setInterval(async () => {
      if (statusPending) return;
      statusPending = true;
      try {
        const data = await settingsUi.upgradeStatus();
        failures = 0;
        if (data.status === 'in_progress') {
          const step = data.step ?? 0;
          currentStep = step <= 2 ? 1 : step === 3 ? 2 : step <= 5 ? 3 : 4;
          currentStatus = data.message ?? '';
        } else if (data.status === 'complete') {
          showSuccess();
        } else if (data.status === 'error') {
          showError(data.message);
        } else if (data.status === 'none' && wentDown) {
          currentStep = 4;
          await probeHealth();
        }
      } catch {
        if (upgradeComplete || upgradeError) return;
        failures++;
        if (failures < 3) {
          currentStatus = 'Reconnecting. This is expected during an upgrade...';
          return;
        }
        wentDown = true;
        currentStep = 4;
        currentStatus = 'Coolify is restarting with the new version...';
        if (statusTimer) clearInterval(statusTimer);
        statusTimer = null;
      } finally {
        statusPending = false;
      }
    }, 2000);
  }

  async function confirmed() {
    showProgress = true;
    startTime = Date.now();
    elapsedTimer = setInterval(() => (elapsedTime = Math.floor((Date.now() - startTime) / 1000)), 1000);
    window.addEventListener('beforeunload', beforeUnload);
    try {
      await settingsUi.upgrade();
      watchStatus();
    } catch (e) {
      showError(e instanceof ApiError ? e.message : String(e));
    }
  }
</script>

<svelte:window onkeydown={(e) => e.key === 'Escape' && !showProgress && (open = false)} />

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
        <div use:portal class="fixed inset-0 z-99 flex min-h-full items-center justify-center overflow-y-auto p-4">
          <div class="absolute inset-0 w-full h-full bg-black/50 backdrop-blur-[2px]"></div>
          <div
            class="application-settings-form application-settings-section relative flex max-h-[calc(100dvh-2rem)] w-full flex-col lg:min-w-[36rem] lg:max-w-2xl"
            style="box-shadow: 0 0 0 1px var(--coollabs-hairline), var(--shadow-modal)"
          >
            <header class="flex-nowrap!">
              <div class="min-w-0 flex-1">
                <h3 class="truncate">{upgradeComplete ? 'Upgrade complete' : showProgress ? 'Upgrading…' : 'Upgrade available'}</h3>
                <p class="mt-0.5 text-[12px] leading-4" style="color: var(--coollabs-subtle)">
                  {upgrade.current_version} <span class="mx-0.5">&rarr;</span>
                  {upgrade.latest_version}
                </p>
              </div>
              {#if !showProgress || upgradeError}
              <button
                type="button"
                onclick={() => (upgradeError ? closeErrorModal() : (open = false))}
                class="flex size-7 shrink-0 cursor-pointer items-center justify-center rounded-md text-neutral-500 outline-0 transition-colors hover:bg-neutral-100 hover:text-black focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-accent dark:text-fg-faint dark:hover:bg-white/[0.06] dark:hover:text-fg"
                aria-label="Close"
              >
                <Reicon name="x" class="size-4" />
              </button>
              {/if}
            </header>
            <div class="application-settings-section-body min-h-0 flex-1 overflow-y-auto" style="-webkit-overflow-scrolling: touch;">
              {#if showProgress}
                <div class="flex flex-col gap-4">
                  <div class="w-full">
                    <div class="grid grid-cols-4 overflow-hidden rounded-[10px] border border-neutral-200 bg-white dark:border-white/[0.08] dark:bg-white/[0.05]">
                      {#each STEPS as label, index (label)}
                        {@const stepNumber = index + 1}
                        <div
                          class="flex min-h-10 items-center justify-center gap-2 border-r border-neutral-200 px-2 text-[11px] font-medium last:border-r-0 dark:border-white/[0.08] sm:px-3 {currentStep === stepNumber ? 'bg-neutral-100 text-neutral-900 dark:bg-white/[0.08] dark:text-fg' : currentStep > stepNumber ? 'text-emerald-600 dark:text-emerald-400' : 'text-neutral-500 dark:text-fg-dim'}"
                        >
                          <span class="flex size-5 shrink-0 items-center justify-center rounded-full border text-[10px] font-semibold {currentStep === stepNumber ? 'border-neutral-300 bg-white dark:border-white/[0.16] dark:bg-white/[0.1]' : currentStep > stepNumber ? 'border-emerald-500/25 bg-emerald-500/10 text-emerald-600 dark:text-emerald-400' : 'border-neutral-200 dark:border-white/[0.1]'}">
                            {#if currentStep > stepNumber}
                              <Reicon name="check-circle" class="size-3" />
                            {:else if currentStep === stepNumber}
                              <svg class="spinner-current size-3 animate-spin text-current" xmlns="http://www.w3.org/2000/svg" fill="none" viewBox="0 0 24 24" aria-hidden="true">
                                <circle class="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" stroke-width="4"></circle>
                                <path class="opacity-75" fill="currentColor" d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4zm2 5.291A7.962 7.962 0 014 12H0c0 3.042 1.135 5.824 3 7.938l3-2.647z"></path>
                              </svg>
                            {:else}
                              <span>{stepNumber}</span>
                            {/if}
                          </span>
                          <span class="hidden truncate sm:inline">{label}</span>
                        </div>
                      {/each}
                    </div>
                  </div>
                  <div class="flex items-center justify-center gap-1.5 text-[12px]" style="color: var(--coollabs-subtle)">
                    <span>Elapsed time</span>
                    <span class="font-mono text-neutral-700 dark:text-fg">{elapsedText}</span>
                  </div>
                  <div class="flex items-center gap-2.5 rounded-lg border border-neutral-200 bg-neutral-50 px-3 py-2.5 dark:border-white/[0.08] dark:bg-white/[0.03]">
                    {#if upgradeComplete}
                      <Reicon name="check-circle" class="size-4 shrink-0 text-emerald-600 dark:text-emerald-400" />
                    {:else if upgradeError}
                      <Reicon name="alert-circle" class="size-4 shrink-0 text-red-600 dark:text-red-400" />
                    {:else}
                      <svg class="loading-indicator size-4 shrink-0 animate-spin" xmlns="http://www.w3.org/2000/svg" fill="none" viewBox="0 0 24 24" aria-hidden="true">
                        <circle class="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" stroke-width="4"></circle>
                        <path class="opacity-75" fill="currentColor" d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4zm2 5.291A7.962 7.962 0 014 12H0c0 3.042 1.135 5.824 3 7.938l3-2.647z"></path>
                      </svg>
                    {/if}
                    <span class="min-w-0 text-[13px] leading-5 text-neutral-700 dark:text-fg">{currentStatus}</span>
                  </div>
                  {#if upgradeComplete}
                    <div class="flex flex-col items-center gap-3">
                      <p class="text-[12px]" style="color: var(--coollabs-subtle)">
                        Reloading in
                        <span class="font-semibold text-coollabs dark:text-brand-light">{successCountdown}</span>
                        seconds…
                      </p>
                      <p class="text-center text-[11px] leading-4" style="color: var(--coollabs-subtle)">
                        If the page does not reload automatically, reload it manually.
                      </p>
                      <button type="button" class="button button-highlighted" onclick={() => window.location.reload()}>Reload now</button>
                    </div>
                  {/if}
                  {#if upgradeError}
                    <div class="flex flex-col gap-3">
                      <p class="text-[12px] leading-5" style="color: var(--coollabs-subtle)">
                        Check the logs on the server at
                        <span class="font-mono text-neutral-700 dark:text-fg">/data/coolify/source/upgrade*</span>.
                      </p>
                      <div class="flex flex-wrap justify-end gap-2 border-t border-neutral-200 pt-4 dark:border-white/[0.08]">
                        <button type="button" class="button" onclick={closeErrorModal}>Close</button>
                      </div>
                    </div>
                  {/if}
                </div>
              {:else}
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
              {/if}
            </div>
          </div>
        </div>
      {/if}
    </div>
  {:else if fullButton}
    <p class="text-sm text-neutral-600 dark:text-fg-dim">Coolify is up to date.</p>
  {/if}
</div>
