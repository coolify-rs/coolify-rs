<script lang="ts">
  // Port of upstream livewire/settings-dropdown.blade.php, trigger account-menu.
  import { untrack } from 'svelte';
  import Reicon from './Reicon.svelte';
  import { request } from '../lib/api';
  import { portal } from '../lib/portal';

  interface Entry {
    tag_name: string;
    title: string;
    content: string;
    content_html: string;
    published_at: string;
    is_read: boolean;
  }

  let { onopen }: { onopen?: () => void } = $props();

  let entries = $state<Entry[]>([]);
  let currentVersion = $state('');
  let open = $state(false);
  let search = $state('');
  let page = $state(1);
  const perPage = 1;

  untrack(async () => {
    try {
      const d = await request<{ entries: Entry[]; current_version: string }>('GET', '/ui/changelog');
      entries = d.entries;
      currentVersion = d.current_version;
    } catch {
      // No changelog files: the modal says there are no updates.
    }
  });

  const unread = $derived(entries.filter((e) => !e.is_read).length);
  const filtered = $derived.by(() => {
    const q = search.trim().toLowerCase();
    return entries
      .filter((e) => !q || e.title.toLowerCase().includes(q) || e.content.toLowerCase().includes(q) || e.tag_name.toLowerCase().includes(q))
      .sort((a, b) => (a.is_read !== b.is_read ? (a.is_read ? 1 : -1) : +new Date(b.published_at) - +new Date(a.published_at)));
  });
  const lastPage = $derived(Math.max(1, Math.ceil(filtered.length / perPage)));
  const visible = $derived(filtered.slice((page - 1) * perPage, page * perPage));
  const first = $derived(filtered.length === 0 ? 0 : (page - 1) * perPage + 1);
  const last = $derived(Math.min(page * perPage, filtered.length));
  const goTo = (p: number) => (page = Math.min(Math.max(p, 1), lastPage));

  function markRead(tag: string) {
    const e = entries.find((x) => x.tag_name === tag);
    if (e) e.is_read = true;
    request('POST', '/ui/changelog/read', { tag }).catch(() => {});
  }

  function markAll() {
    for (const e of entries) e.is_read = true;
    request('POST', '/ui/changelog/read-all', {}).catch(() => {});
  }

  const iconPath =
    'M9.813 15.904 9 18.75l-.813-2.846a4.5 4.5 0 0 0-3.091-3.091L2.25 12l2.846-.813a4.5 4.5 0 0 0 3.091-3.091L9 5.25l.813 2.846a4.5 4.5 0 0 0 3.091 3.091L15.75 12l-2.846.813a4.5 4.5 0 0 0-3.091 3.091ZM18.259 8.715 18 9.75l-.259-1.035a3.375 3.375 0 0 0-2.456-2.456L14.25 6l1.035-.259a3.375 3.375 0 0 0 2.456-2.456L18 2.25l.259 1.035a3.375 3.375 0 0 0 2.456 2.456L21.75 6l-1.035.259a3.375 3.375 0 0 0-2.456 2.456Z';
  const pagerButton =
    'flex w-10 items-center justify-center border-r border-neutral-200 text-neutral-500 transition-colors hover:bg-neutral-100 disabled:cursor-not-allowed disabled:opacity-35 dark:border-white/[0.10] dark:text-fg-dim dark:hover:bg-white/[0.06]';
</script>

<svelte:window onkeydown={(e) => e.key === 'Escape' && (open = false)} />

<div class="w-full">
  <button
    type="button"
    class="listbox-option relative w-full text-left"
    onclick={() => {
      open = true;
      onopen?.();
    }}
  >
    <span class="flex items-center gap-2">
      <svg class="size-4 opacity-80" fill="none" stroke="currentColor" viewBox="0 0 24 24" aria-hidden="true">
        <path stroke-linecap="round" stroke-linejoin="round" stroke-width="1.8" d={iconPath} />
      </svg>
      What's New
    </span>
    {#if unread > 0}
      <span
        class="ml-auto flex h-4 min-w-4 items-center justify-center rounded-full bg-error px-1 text-[10px] text-white"
        aria-label="{unread} unread changelog {unread === 1 ? 'entry' : 'entries'}"
      >
        {unread > 9 ? '9+' : unread}
      </span>
    {/if}
  </button>

  {#if open}
    <div use:portal class="fixed inset-0 z-[99] flex items-center justify-center p-3 sm:p-6">
      <div class="absolute inset-0 bg-black/55 backdrop-blur-[2px]" role="presentation" onclick={() => (open = false)}></div>
      <section
        class="application-settings-form application-settings-section relative flex max-h-[calc(100dvh-3rem)] !w-full max-w-5xl flex-col overflow-hidden"
        style="box-shadow: 0 0 0 1px var(--coollabs-hairline), var(--shadow-modal)"
      >
        <header class="relative !pr-11">
          <div class="flex min-w-0 items-center gap-3">
            <div class="flex size-8 shrink-0 items-center justify-center rounded-lg bg-neutral-100 text-neutral-500 dark:bg-white/[0.05] dark:text-fg-dim">
              <svg class="size-4" fill="none" stroke="currentColor" viewBox="0 0 24 24" aria-hidden="true">
                <path stroke-linecap="round" stroke-linejoin="round" stroke-width="1.8" d={iconPath} />
              </svg>
            </div>
            <div class="min-w-0">
              <div class="flex flex-wrap items-center gap-2">
                <h3>What's new</h3>
                <span class="rounded-md bg-neutral-100 px-1.5 py-0.5 text-[11px] font-medium text-neutral-500 dark:bg-white/[0.05] dark:text-fg-faint">{currentVersion}</span>
              </div>
              <p class="mt-0.5 text-xs font-normal text-neutral-500 dark:text-fg-faint">Product updates, fixes, and improvements.</p>
            </div>
          </div>
          <div class="flex items-center gap-2">
            {#if unread > 0}
              <button type="button" class="button" onclick={markAll}>Mark all read</button>
            {/if}
          </div>
          <button
            type="button"
            onclick={() => (open = false)}
            class="absolute right-2 top-2 flex size-7 cursor-pointer items-center justify-center rounded-md text-neutral-500 outline-0 hover:bg-neutral-100 hover:text-black focus-visible:ring-1 focus-visible:ring-accent dark:text-fg-faint dark:hover:bg-white/[0.06] dark:hover:text-fg"
            aria-label="Close What's new"
          >
            <Reicon name="x" class="size-4" />
          </button>
        </header>

        <div class="application-settings-section-body flex min-h-0 flex-1 flex-col !p-0">
          <div class="flex shrink-0 items-center gap-3 border-b border-neutral-200 p-3 dark:border-white/[0.06]">
            <div class="relative w-full max-w-sm">
              <Reicon name="search" class="pointer-events-none absolute left-3 top-1/2 size-4 -translate-y-1/2 text-neutral-400" />
              <input bind:value={search} oninput={() => (page = 1)} type="search" placeholder="Search updates" class="input !pl-9" />
            </div>
            <span class="ml-auto hidden text-xs text-neutral-500 sm:block dark:text-fg-faint">{filtered.length} {filtered.length === 1 ? 'update' : 'updates'}</span>
          </div>

          <div class="min-h-0 flex-1 overflow-y-auto p-4 scrollbar sm:p-5">
            {#if filtered.length > 0}
              <div>
                {#each visible as entry (entry.tag_name)}
                  <article>
                    <div class="mb-4 flex items-start gap-4">
                      <div class="min-w-0 flex-1">
                        <div class="flex flex-wrap items-center gap-x-2 gap-y-1">
                          <a
                            href="https://github.com/coollabsio/coolify/releases/tag/{entry.tag_name}"
                            target="_blank"
                            rel="noopener noreferrer"
                            class="inline-flex min-w-0 items-center gap-1.5 text-sm font-semibold text-black hover:text-accent dark:text-fg"
                          >
                            <span class="truncate">{entry.title}</span>
                            <svg class="inline-flex w-3 h-3 dark:text-neutral-400 text-black" fill="none" stroke="currentColor" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M10 6H6a2 2 0 00-2 2v10a2 2 0 002 2h10a2 2 0 002-2v-4M14 4h6m0 0v6m0-6L10 14" /></svg>
                          </a>
                          {#if entry.tag_name === currentVersion}
                            <span class="rounded-md bg-success/10 px-1.5 py-0.5 text-[10px] font-semibold uppercase tracking-wide text-success">Current</span>
                          {/if}
                          <span class="text-xs text-neutral-500 dark:text-fg-faint">
                            {new Date(entry.published_at).toLocaleDateString('en-US', { month: 'short', day: 'numeric', year: 'numeric' })}
                          </span>
                        </div>
                      </div>
                      {#if !entry.is_read}
                        <button
                          type="button"
                          onclick={() => markRead(entry.tag_name)}
                          class="shrink-0 cursor-pointer rounded-md px-2 py-1 text-xs font-medium text-neutral-500 hover:bg-neutral-100 hover:text-black dark:text-fg-faint dark:hover:bg-white/[0.06] dark:hover:text-fg"
                          title="Mark as read"
                        >
                          Mark read
                        </button>
                      {/if}
                    </div>
                    <div
                      class="max-w-none text-sm leading-6 text-neutral-600 dark:text-fg-dim [&_a]:font-medium [&_a]:text-accent [&_code]:rounded [&_code]:bg-neutral-100 [&_code]:px-1 [&_code]:py-0.5 dark:[&_code]:bg-white/[0.06] [&_h2]:mb-2 [&_h2]:mt-5 [&_h2]:text-base [&_h2]:font-semibold [&_h2]:text-black dark:[&_h2]:text-fg [&_h3]:mb-2 [&_h3]:mt-4 [&_h3]:text-sm [&_h3]:font-semibold [&_h3]:text-black dark:[&_h3]:text-fg [&_li]:my-1 [&_ul]:ml-5 [&_ul]:list-disc"
                    >
                      <!-- eslint-disable-next-line svelte/no-at-html-tags -->
                      {@html entry.content_html}
                    </div>
                  </article>
                {/each}
              </div>
            {:else}
              <div class="flex min-h-64 flex-col items-center justify-center px-6 py-12 text-center">
                <div class="mb-3 flex size-10 items-center justify-center rounded-lg bg-neutral-100 text-neutral-400 dark:bg-white/[0.05] dark:text-fg-faint">
                  <Reicon name="documentation" class="size-5" />
                </div>
                <h3 class="text-sm font-medium text-black dark:text-fg">No updates found</h3>
                <p class="mt-1 max-w-sm text-sm text-neutral-500 dark:text-fg-faint">
                  {#if search.trim() !== ''}<span>No releases match your search.</span>{:else}<span>There are no product updates available yet.</span>{/if}
                </p>
              </div>
            {/if}
          </div>
          {#if filtered.length > 0}
            <div class="flex min-h-14 shrink-0 items-center justify-between gap-3 border-t border-neutral-200 px-4 py-3 dark:border-white/[0.08]">
              <p class="text-[13px] text-neutral-500 dark:text-fg-dim">
                Showing
                <span class="tabular-nums text-black dark:text-fg">{first}–{last}</span>
                of
                <span class="tabular-nums text-black dark:text-fg">{filtered.length}</span>
              </p>
              <div class="inline-flex h-8 overflow-hidden rounded-lg border border-neutral-200 dark:border-white/[0.10]">
                <button type="button" class={pagerButton} aria-label="First page" title="First page" onclick={() => goTo(1)} disabled={page === 1}>
                  <svg class="size-3.5" viewBox="0 0 24 24" fill="none" aria-hidden="true"><path d="M18 6L12 12L18 18M11 6L5 12L11 18" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" /></svg>
                </button>
                <button type="button" class={pagerButton} aria-label="Previous page" title="Previous page" onclick={() => goTo(page - 1)} disabled={page === 1}>
                  <svg class="size-3.5" viewBox="0 0 24 24" fill="none" aria-hidden="true"><path d="M15 6L9 12L15 18" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" /></svg>
                </button>
                <span class="flex min-w-12 items-center justify-center border-r border-neutral-200 px-3 text-[13px] tabular-nums text-black dark:border-white/[0.10] dark:text-fg">{page}</span>
                <button type="button" class={pagerButton} aria-label="Next page" title="Next page" onclick={() => goTo(page + 1)} disabled={page === lastPage}>
                  <svg class="size-3.5" viewBox="0 0 24 24" fill="none" aria-hidden="true"><path d="M9 6L15 12L9 18" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" /></svg>
                </button>
                <button
                  type="button"
                  class="flex w-10 items-center justify-center text-neutral-500 transition-colors hover:bg-neutral-100 disabled:cursor-not-allowed disabled:opacity-35 dark:text-fg-dim dark:hover:bg-white/[0.06]"
                  aria-label="Last page"
                  title="Last page"
                  onclick={() => goTo(lastPage)}
                  disabled={page === lastPage}
                >
                  <svg class="size-3.5" viewBox="0 0 24 24" fill="none" aria-hidden="true"><path d="M6 6L12 12L6 18M13 6L19 12L13 18" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" /></svg>
                </button>
              </div>
            </div>
          {/if}
        </div>
      </section>
    </div>
  {/if}
</div>
