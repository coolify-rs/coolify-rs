<script lang="ts">
  // Port of upstream livewire/layout-popups.blade.php and components/popup.blade.php. The real-time
  // warning is left out: coolify-rs has no Echo connection, and upstream shows it only when one fails.
  import Reicon from './Reicon.svelte';
  import { layoutUi } from '../lib/layoutApi';
  import { link } from '../lib/router.svelte';
  import { session } from '../lib/session.svelte';

  const COLLAPSE_AFTER = 10000;

  /** `shouldShowMonthlyPopup`. */
  function monthly(key: string): boolean {
    let stored: string | null = null;
    try {
      stored = localStorage.getItem(key);
    } catch {
      return true;
    }
    if (!stored || stored === 'false') return true;
    const time = parseInt(stored);
    if (isNaN(time)) return true;
    const now = new Date();
    const then = new Date(time);
    return now.getMonth() !== then.getMonth() || now.getFullYear() !== then.getFullYear();
  }

  function remember(key: string) {
    try {
      localStorage.setItem(key, Date.now().toString());
    } catch {
      // Storage may be unavailable; the popup comes back on the next load.
    }
  }

  let sponsorshipEnabled = $state(false);
  let notificationDisabled = $state(false);
  let sponsorship = $state(monthly('popupSponsorship'));
  let notification = $state(monthly('popupNotification'));
  let visible = $state(false);
  let sponsorshipCompact = $state(false);
  let notificationCompact = $state(false);

  // `currentTeam()` decides; a team switch reloads the data.
  $effect(() => {
    void session.info?.current_team?.id;
    layoutUi.layout().then((d) => {
      sponsorshipEnabled = !!d.is_sponsorship_popup_enabled;
      notificationDisabled = !d.any_notification_enabled;
    });
  });

  $effect(() => {
    // `bannerVisibleAfter: 300`.
    const show = setTimeout(() => (visible = true), 300);
    const a = setTimeout(() => (sponsorshipCompact = true), COLLAPSE_AFTER);
    const b = setTimeout(() => (notificationCompact = true), COLLAPSE_AFTER);
    return () => {
      clearTimeout(show);
      clearTimeout(a);
      clearTimeout(b);
    };
  });

  const banner = 'fixed bottom-0 right-0 w-full h-auto duration-300 ease-out sm:px-5 sm:pb-5 w-full z-999';
  const close =
    'absolute top-3 right-3 flex size-7 items-center justify-center rounded-full text-neutral-400 transition-colors hover:bg-neutral-100 hover:text-black dark:text-fg-faint dark:hover:bg-white/[0.07] dark:hover:text-fg';
  const shell =
    'relative mx-auto flex w-full flex-col overflow-hidden rounded-2xl border border-neutral-200 bg-white shadow-modal transition-all duration-300 dark:border-white/[0.1] dark:bg-surface';
</script>

{#if visible && sponsorshipEnabled && sponsorship}
  <span>
    <div class={banner}>
      <div class="{shell} {sponsorshipCompact ? 'max-w-sm gap-3 p-4' : 'max-w-2xl gap-5 p-5 sm:p-6'}">
        <button
          type="button"
          aria-label="Dismiss sponsorship reminder"
          class={close}
          onclick={() => {
            sponsorship = false;
            remember('popupSponsorship');
          }}
        >
          <Reicon name="x" class="size-3.5" />
        </button>
        <div class="flex items-start gap-4 pr-8">
          {#if !sponsorshipCompact}
            <div class="hidden size-12 shrink-0 items-center justify-center rounded-xl border border-neutral-200 bg-neutral-50 sm:flex dark:border-white/[0.08] dark:bg-white/[0.04]">
              <img src="/heart.png" alt="" class="size-9" />
            </div>
          {/if}
          <div class="min-w-0">
            <h2 class="text-[15px]! leading-5! font-semibold! text-black dark:text-fg">Love Coolify? Support our work.</h2>
            {#if !sponsorshipCompact}
              <p class="mt-1 text-[12px] leading-5 text-neutral-500 dark:text-fg-dim">
                Coolify is profitable thanks to <span class="font-semibold text-coollabs dark:text-brand-light">you</span>. Your support helps us build more features and keep improving the project.
              </p>
            {/if}
          </div>
        </div>
        <div class="flex flex-col gap-2 sm:flex-row sm:items-center">
          <a target="_blank" href="https://github.com/sponsors/coollabsio" class="button button-highlighted h-9 justify-center sm:flex-1">GitHub Sponsors</a>
          {#if !sponsorshipCompact}
            <a target="_blank" href="https://opencollective.com/coollabsio/donate?interval=month&amount=10&name=&legalName=&email=" class="button h-9 justify-center sm:flex-1">Open Collective</a>
            <a href="https://donate.stripe.com/8x2bJ104ifmB9kB45u38402" target="_blank" class="button h-9 justify-center sm:flex-1">Stripe</a>
            <button
              type="button"
              class="h-9 cursor-pointer px-2 text-[12px] font-medium text-neutral-500 transition-colors hover:text-black sm:shrink-0 dark:text-fg-dim dark:hover:text-fg"
              onclick={() => {
                sponsorship = false;
                remember('popupSponsorship');
              }}
            >
              Maybe next time
            </button>
          {/if}
        </div>
      </div>
    </div>
  </span>
{/if}

{#if visible && notificationDisabled && notification}
  <span>
    <div class={banner}>
      <div class="{shell} {notificationCompact ? 'max-w-sm gap-3 p-4' : 'max-w-2xl gap-5 p-5 sm:p-6'}">
        <button
          type="button"
          aria-label="Dismiss notifications reminder"
          class={close}
          onclick={() => {
            notification = false;
            remember('popupNotification');
          }}
        >
          <Reicon name="x" class="size-3.5" />
        </button>
        <div class="flex items-start gap-4 pr-8">
          {#if !notificationCompact}
            <div class="hidden size-12 shrink-0 items-center justify-center rounded-xl border border-amber-200 bg-amber-50 text-amber-700 sm:flex dark:border-warning/20 dark:bg-warning/10 dark:text-warning">
              <Reicon name="alert-triangle" class="size-6" />
            </div>
          {/if}
          <div class="min-w-0">
            <h2 class="text-[15px]! leading-5! font-semibold! text-black dark:text-fg">No notifications enabled</h2>
            {#if !notificationCompact}
              <p class="mt-1 text-[12px] leading-5 text-neutral-500 dark:text-fg-dim">
                Enable at least one notification channel so you receive important alerts. Visit
                <a
                  href="/notifications/email"
                  use:link
                  class="font-medium text-coollabs underline decoration-coollabs/30 underline-offset-2 transition-colors hover:text-coollabs-100 dark:text-brand-light dark:decoration-brand-light/30 dark:hover:text-brand-light/90">notifications</a
                >
                to get started.
              </p>
            {/if}
          </div>
        </div>
        <div class="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-end">
          <a href="/notifications/email" use:link class="button h-9 justify-center sm:min-w-28">Open notifications</a>
          {#if !notificationCompact}
            <button
              type="button"
              class="button h-9 justify-center sm:min-w-32"
              onclick={() => {
                notification = false;
                remember('popupNotification');
              }}
            >
              Accept and close
            </button>
          {/if}
        </div>
      </div>
    </div>
  </span>
{/if}
