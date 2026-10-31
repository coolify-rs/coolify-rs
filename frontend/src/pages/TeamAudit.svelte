<script lang="ts">
  // Port of upstream livewire/team/audit-log.blade.php (App\Livewire\Team\AuditLog).
  import Reicon from '../components/Reicon.svelte';
  import Empty from '../components/Empty.svelte';
  import Listbox from '../components/Listbox.svelte';
  import Pager from '../components/Pager.svelte';
  import SettingsSection from '../components/SettingsSection.svelte';
  import TeamLayout from '../components/TeamLayout.svelte';
  import { teamsUi, type AuditData } from '../lib/teamsApi';
  import { diffForHumans, parseTimestamp } from '../lib/format';
  import { router } from '../lib/router.svelte';

  let data = $state<AuditData | null>(null);
  let search = $state('');
  let action = $state('all');
  let source = $state('all');
  let page = $state(1);
  let perPage = $state(25);
  let expanded = $state<Record<number, boolean>>({});
  let timer: ReturnType<typeof setTimeout> | undefined;

  async function load() {
    try {
      data = await teamsUi.audit({ search, action, source, per_page: perPage, page });
    } catch {
      router.go('/', true);
    }
  }

  $effect(() => {
    void [action, source, page, perPage];
    load();
  });

  function onSearch() {
    clearTimeout(timer);
    timer = setTimeout(() => {
      if (page === 1) load();
      else page = 1;
    }, 300);
  }

  /** `Str::headline`. */
  function headline(s: string): string {
    return s
      .replace(/([a-z])([A-Z])/g, '$1 $2')
      .split(/[\s_.-]+/)
      .filter(Boolean)
      .map((w) => w.charAt(0).toUpperCase() + w.slice(1).toLowerCase())
      .join(' ');
  }

  const MONTHS = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];
  const DAYS = ['Sun', 'Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat'];

  /** Carbon `toDayDateTimeString()` in UTC. */
  function dayDateTime(ts: string): string {
    const d = parseTimestamp(ts);
    if (!d) return '';
    const h = d.getUTCHours();
    return `${DAYS[d.getUTCDay()]}, ${MONTHS[d.getUTCMonth()]} ${d.getUTCDate()}, ${d.getUTCFullYear()} ${h % 12 || 12}:${String(d.getUTCMinutes()).padStart(2, '0')} ${h < 12 ? 'AM' : 'PM'}`;
  }

  /** Carbon `toIso8601String()` in UTC. */
  function iso(ts: string): string {
    return parseTimestamp(ts)?.toISOString().replace(/\.\d{3}Z$/, '+00:00') ?? '';
  }

  function value(v: unknown): string {
    if (Array.isArray(v) || (typeof v === 'object' && v !== null)) return JSON.stringify(v);
    if (typeof v === 'boolean') return v ? 'True' : 'False';
    return v === null || v === undefined ? 'None' : String(v);
  }
</script>

<div>
  <TeamLayout>
    <div class="application-settings-form">
      <SettingsSection title="Audit log" description="Activity from the last 90 days for the current team." flush>
        <div class="flex flex-col gap-2 border-b border-neutral-200 p-3 sm:flex-row sm:items-center dark:border-white/[0.08]">
          <div class="relative min-w-0 flex-1 sm:max-w-sm">
            <Reicon name="search" class="pointer-events-none absolute top-1/2 left-2.5 z-10 size-3.5 -translate-y-1/2 text-neutral-400 dark:text-fg-faint" />
            <input
              bind:value={search}
              oninput={onSearch}
              type="search"
              placeholder="Search activity"
              aria-label="Search activity"
              class="h-8! w-full rounded-lg! border-neutral-200! bg-white! py-0! pr-8! pl-8! text-[12px]! shadow-none! placeholder:text-neutral-400 focus:border-accent! focus:ring-0! dark:border-white/[0.08]! dark:bg-white/[0.035]! dark:text-fg! dark:placeholder:text-fg-faint"
            />
          </div>
          <div class="grid grid-cols-2 gap-2 sm:flex">
            <div class="sm:w-36">
              <Listbox id="action" bind:value={action} onchange={() => (page = 1)} options={[{ value: 'all', label: 'All actions' }, ...(data?.actions ?? [])]} />
            </div>
            <div class="sm:w-36">
              <Listbox
                id="source"
                bind:value={source}
                onchange={() => (page = 1)}
                options={[
                  { value: 'all', label: 'All sources' },
                  { value: 'ui', label: 'Web UI' },
                  { value: 'api', label: 'API' },
                  { value: 'mcp', label: 'MCP' },
                  { value: 'webhook', label: 'Webhook' },
                ]}
              />
            </div>
          </div>
        </div>

        {#if data && data.events.length > 0}
          <div class="overflow-x-auto">
            <div class="data-table transition-opacity">
              <div class="grid min-w-[760px] grid-cols-[14rem_minmax(0,1fr)_12rem_9rem] gap-4 border-b border-neutral-200 px-4 py-2 text-[11px] font-medium uppercase tracking-wide text-neutral-500 dark:border-white/[0.07] dark:text-fg-faint">
                <span>Actor</span>
                <span>Activity</span>
                <span>Source</span>
                <span class="text-right">Time</span>
              </div>
              {#each data.events as event (event.id)}
                {@const changes = event.changes && Object.keys(event.changes).length ? event.changes : null}
                <div class="min-w-[760px] border-b border-neutral-200 last:border-b-0 dark:border-white/[0.07]">
                  <div class="grid grid-cols-[14rem_minmax(0,1fr)_12rem_9rem] gap-4 px-4 py-3">
                    <div class="min-w-0">
                      <div class="truncate text-[12px] font-medium text-black dark:text-fg">{event.actor_name || headline(event.actor_type)}</div>
                      {#if event.actor_email}
                        <div class="truncate text-[11px] text-neutral-500 dark:text-fg-faint">{event.actor_email}</div>
                      {/if}
                      {#if event.actor_token_name}
                        <div class="truncate text-[11px] text-neutral-500 dark:text-fg-faint" title="Token: {event.actor_token_name}">Token: {event.actor_token_name}</div>
                      {/if}
                    </div>
                    <div class="min-w-0">
                      <div class="truncate text-[13px] font-medium text-black dark:text-fg">{event.description}</div>
                      <div class="mt-0.5 truncate text-[11px] text-neutral-500 dark:text-fg-faint">{event.event}</div>
                      {#if changes}
                        <button
                          type="button"
                          onclick={() => (expanded[event.id] = !expanded[event.id])}
                          class="mt-1 text-[11px] font-medium text-accent hover:underline"
                          aria-expanded={!!expanded[event.id]}
                        >
                          <span>{expanded[event.id] ? 'Hide changes' : 'View changes'}</span>
                        </button>
                      {/if}
                    </div>
                    <div class="flex items-center gap-2 text-[12px] text-neutral-500 dark:text-fg-dim">
                      <span class="rounded-md bg-neutral-100 px-2 py-1 dark:bg-white/[0.06]">{event.source.toUpperCase()}</span>
                      <span>{headline(event.action)}</span>
                    </div>
                    <time datetime={iso(event.created_at)} title={dayDateTime(event.created_at)} class="self-center text-right text-[11px] text-neutral-500 dark:text-fg-faint">
                      {diffForHumans(event.created_at)}
                    </time>
                  </div>
                  {#if changes && expanded[event.id]}
                    <div class="border-t border-neutral-200 bg-neutral-50 px-4 py-3 dark:border-white/[0.07] dark:bg-white/[0.02]">
                      <div class="grid grid-cols-[12rem_minmax(0,1fr)_minmax(0,1fr)] gap-4 pb-2 text-[10px] font-medium uppercase tracking-wide text-neutral-500 dark:text-fg-faint">
                        <span>Field</span>
                        <span>Previous value</span>
                        <span>New value</span>
                      </div>
                      {#each Object.entries(changes) as [field, change] (field)}
                        <div class="grid grid-cols-[12rem_minmax(0,1fr)_minmax(0,1fr)] gap-4 border-t border-neutral-200 py-2 text-[12px] dark:border-white/[0.07]">
                          <span class="font-medium text-black dark:text-fg">{headline(field)}</span>
                          <span class="break-words whitespace-pre-wrap text-neutral-600 dark:text-fg-dim">{value(change.old)}</span>
                          <span class="break-words whitespace-pre-wrap text-neutral-600 dark:text-fg-dim">{value(change.new)}</span>
                        </div>
                      {/each}
                    </div>
                  {/if}
                </div>
              {/each}
            </div>
          </div>
          <Pager total={data.total} bind:page bind:pageSize={perPage} storageKey="coolify.page-size.audit-log" separator="–" />
        {:else if data}
          <Empty title="No activity found" description="Team actions will appear here as they happen." iconName="time-back" size="sm" />
        {/if}
      </SettingsSection>
    </div>
  </TeamLayout>
</div>
