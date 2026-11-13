<script lang="ts">
  // Port of upstream components/image-scan/gate-status.blade.php: the "Blocked" or "Not scanned"
  // badge of a deployment, with a popover that says why, links to the Security page, and offers
  // "Deploy anyway".
  import Reicon from '../Reicon.svelte';
  import StatusBadge from '../StatusBadge.svelte';
  import { link } from '../../lib/router.svelte';

  let {
    result = {},
    vulnerabilitiesUrl = null,
    deploymentUrl = null,
    showDeployAnyway = false,
    canDeployAnyway = false,
    action = null,
    ondeployanyway,
    class: cls = '',
  }: {
    result?: Record<string, unknown> | null;
    vulnerabilitiesUrl?: string | null;
    deploymentUrl?: string | null;
    showDeployAnyway?: boolean;
    canDeployAnyway?: boolean;
    /** The sentence of the primary action of the Security page for the blocked image. */
    action?: string | null;
    ondeployanyway?: () => void;
    class?: string;
  } = $props();

  const filled = (v: unknown) => v !== null && v !== undefined && v !== false && String(v).trim() !== '' && !(Array.isArray(v) && v.length === 0);
  const isBlocked = $derived(!!result?.blocked && result.blocked !== '0');
  const isSkipped = $derived(!isBlocked && filled(result?.error));
  const headline = $derived(String(result?.headline ?? ''));
  const linkClasses =
    'inline-flex items-center gap-0.5 font-semibold normal-case text-coollabs transition-colors hover:text-coollabs-100 dark:text-brand-light dark:hover:text-brand-light/80';

  let open = $state(false);
  let panelStyle = $state('position: fixed; visibility: hidden;');
  let root: HTMLDivElement | undefined = $state();
  let trigger: HTMLSpanElement | undefined = $state();
  let panel: HTMLDivElement | undefined = $state();

  function updatePosition() {
    if (!trigger || !panel) return;
    const t = trigger.getBoundingClientRect();
    const p = panel.getBoundingClientRect();
    const padding = 8;
    const left = Math.max(padding, Math.min(t.left, window.innerWidth - p.width - padding));
    const top = window.innerHeight - t.bottom - padding >= p.height ? t.bottom + 6 : Math.max(padding, t.top - p.height - 6);
    panelStyle = `position: fixed; left: ${left}px; top: ${top}px;`;
  }
  function toggle() {
    if (open) {
      open = false;
      return;
    }
    panelStyle = 'position: fixed; visibility: hidden;';
    open = true;
    requestAnimationFrame(() => updatePosition());
  }
</script>

<svelte:window
  onclick={(e) => open && root && !root.contains(e.target as Node) && (open = false)}
  onkeydown={(e) => e.key === 'Escape' && (open = false)}
  onresize={() => open && updatePosition()}
  onscroll={() => open && updatePosition()}
/>

{#if isBlocked || isSkipped}
  <div data-image-scan-gate class="relative {cls}" bind:this={root}>
    <span bind:this={trigger}>
      <StatusBadge
        as="button"
        onclick={toggle}
        label={isBlocked ? 'Blocked' : 'Not scanned'}
        type={isBlocked ? 'error' : 'warning'}
        class="hover:bg-neutral-200 dark:hover:bg-white/[0.12]"
      />
    </span>

    {#if open}
      <div
        bind:this={panel}
        style={panelStyle}
        role="dialog"
        aria-label={isBlocked ? 'Deploy blocked' : 'Image not scanned'}
        class="surface-popover z-[1100] w-[calc(100vw-2rem)] max-w-sm rounded-lg p-3 text-left"
      >
        <div class="flex items-start gap-2.5">
          <span
            class="flex size-7 shrink-0 items-center justify-center rounded-md {isBlocked
              ? 'bg-red-100 text-red-700 dark:bg-error/10 dark:text-error'
              : 'bg-amber-100 text-amber-700 dark:bg-warning/10 dark:text-warning'}"
          >
            <Reicon name="shield-alert" class="size-4" />
          </span>
          <div class="min-w-0 flex-1 whitespace-normal">
            <p class="text-[13px] font-semibold leading-4 text-neutral-950 dark:text-fg">{headline}</p>
            {#if isBlocked && action}
              <p class="mt-1 text-[11px] leading-4 text-neutral-600 dark:text-fg-dim">{action}</p>
            {/if}
            <div class="mt-2 flex flex-wrap items-center gap-x-3 gap-y-1.5 text-[11px] leading-4">
              {#if vulnerabilitiesUrl}
                <a href={vulnerabilitiesUrl} use:link class={linkClasses}>
                  Open Security
                  <Reicon name="arrow-right" class="size-2.5" />
                </a>
              {/if}
              {#if deploymentUrl}
                <a href={deploymentUrl} use:link class={linkClasses}>
                  Open deployment
                  <Reicon name="arrow-right" class="size-2.5" />
                </a>
              {/if}
              {#if isBlocked && showDeployAnyway && canDeployAnyway}
                <button type="button" class="button normal-case!" onclick={() => ((open = false), ondeployanyway?.())}>Deploy anyway</button>
              {/if}
            </div>
            {#if isBlocked && showDeployAnyway && !canDeployAnyway}
              <p class="mt-2 text-[11px] leading-4 text-neutral-600 dark:text-fg-dim">Ask someone with write access to deploy anyway.</p>
            {/if}
          </div>
        </div>
      </div>
    {/if}
  </div>
{/if}
