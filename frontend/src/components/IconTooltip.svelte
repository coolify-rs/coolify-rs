<script lang="ts">
  // Port of upstream components/icon-tooltip.blade.php: one tooltip for the whole page. It shows the
  // label of icon-only buttons and links and of anything with `data-tooltip`, in place of the
  // browser's `title` tooltip.
  import { tick } from 'svelte';

  let visible = $state(false);
  let positioned = $state(false);
  let text = $state('');
  let x = $state(0);
  let y = $state(0);
  let below = $state(false);
  let wrap = $state(false);
  let activeTarget: HTMLElement | null = null;
  let tooltip: HTMLDivElement | undefined = $state();

  const label = (target: HTMLElement) => target.dataset.tooltip || target.getAttribute('aria-label') || target.title;
  // Close and dismiss (X) buttons are obvious, so they get no tooltip.
  const isDismiss = (target: HTMLElement) => /^(close|dismiss)\b/i.test(label(target) || '');

  function isIconAction(target: HTMLElement): boolean {
    if (target.matches('[data-icon-tooltip-ignore]')) return false;
    if (isDismiss(target)) return false;
    if (target.matches('[data-tooltip], .icon-button')) return true;
    return target.hasAttribute('aria-label') && target.childElementCount === 1 && !!target.firstElementChild?.matches('svg');
  }

  function prepare(root: ParentNode | null) {
    root?.querySelectorAll?.<HTMLElement>('button[title], a[title], [data-tooltip]').forEach((target) => {
      const dismiss = isDismiss(target);
      if (!dismiss && !isIconAction(target)) return;
      const text = label(target);
      if (!dismiss) target.dataset.iconTooltip = text;
      if (!target.getAttribute('aria-label')) target.setAttribute('aria-label', text);
      target.removeAttribute('title');
    });
  }

  function findTarget(event: Event): HTMLElement | null {
    const target = (event.target as Element | null)?.closest?.<HTMLElement>('button, a, [data-tooltip]') ?? null;
    if (target?.matches('.menu-item, .menu-subitem') && target.closest('nav')) return null;
    return target && isIconAction(target) ? target : null;
  }

  async function show(event: Event) {
    const target = findTarget(event);
    if (!target) return;
    if (target === activeTarget && visible) return;
    const content = target.dataset.tooltip || target.dataset.iconTooltip || target.getAttribute('aria-label');
    if (!content) return;
    activeTarget = target;
    text = content;
    // Long sentences opt in to wrapping with data-tooltip-wrap; short labels stay on one line.
    wrap = target.hasAttribute('data-tooltip-wrap');
    positioned = false;
    visible = true;
    const rect = target.getBoundingClientRect();
    below = rect.top < 48;
    x = rect.left + rect.width / 2;
    y = below ? rect.bottom + 8 : rect.top - 8;
    await tick();
    if (activeTarget !== target) return;
    const width = tooltip?.offsetWidth || 0;
    x = Math.max(8, Math.min(window.innerWidth - width - 8, x - width / 2));
    await tick();
    if (activeTarget === target) positioned = true;
  }

  function hide(event?: Event) {
    const related = (event as MouseEvent | FocusEvent | undefined)?.relatedTarget as Node | null | undefined;
    if (related && activeTarget?.contains(related)) return;
    visible = false;
    positioned = false;
    activeTarget = null;
  }

  $effect(() => {
    prepare(document);
    const observer = new MutationObserver((mutations) =>
      mutations.forEach((mutation) =>
        mutation.addedNodes.forEach((node) => {
          if (!(node instanceof HTMLElement)) return;
          prepare(node);
          if (node.matches('button[title], a[title], [data-tooltip]')) prepare(node.parentElement);
        }),
      ),
    );
    observer.observe(document.body, { childList: true, subtree: true });
    // Focus events also fire while Svelte removes a focused element (a page change), inside its
    // update, where writing state throws `state_unsafe_mutation`; handle them after the update.
    const onShow = (event: Event) => queueMicrotask(() => void show(event));
    const onHide = (event: Event) => queueMicrotask(() => hide(event));
    const onReset = () => hide();
    document.addEventListener('mouseover', onShow);
    document.addEventListener('mouseout', onHide);
    document.addEventListener('focusin', onShow);
    document.addEventListener('focusout', onHide);
    document.addEventListener('click', onReset);
    window.addEventListener('scroll', onReset, true);
    window.addEventListener('resize', onReset);
    return () => {
      observer.disconnect();
      document.removeEventListener('mouseover', onShow);
      document.removeEventListener('mouseout', onHide);
      document.removeEventListener('focusin', onShow);
      document.removeEventListener('focusout', onHide);
      document.removeEventListener('click', onReset);
      window.removeEventListener('scroll', onReset, true);
      window.removeEventListener('resize', onReset);
    };
  });
</script>

<div class="contents">
  <div
    bind:this={tooltip}
    role="tooltip"
    hidden={!visible}
    style="left: {x}px; top: {y}px;"
    class="pointer-events-none fixed z-[10000] rounded-lg border border-neutral-700 bg-neutral-900 px-2 py-1 text-xs font-medium text-white shadow-dropdown dark:border-white/10 dark:bg-raised {below
      ? ''
      : '-translate-y-full'} {positioned ? 'visible' : 'invisible'} {wrap ? 'max-w-xs whitespace-normal' : 'whitespace-nowrap'}"
  >
    {text}
  </div>
</div>
