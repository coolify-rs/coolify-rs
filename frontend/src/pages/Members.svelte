<script lang="ts">
  // Port of upstream livewire/team/member/index.blade.php with
  // livewire/team/member.blade.php. Invitations are not available yet.
  import Reicon from '../components/Reicon.svelte';
  import Empty from '../components/Empty.svelte';
  import Listbox from '../components/Listbox.svelte';
  import Pager from '../components/Pager.svelte';
  import SettingsSection from '../components/SettingsSection.svelte';
  import TeamLayout from '../components/TeamLayout.svelte';
  import { teamUi, ApiError, type Role, type TeamPageData } from '../lib/api';
  import { notify } from '../lib/toast.svelte';

  type M = TeamPageData['members'][number];

  let data = $state<TeamPageData | null>(null);
  let search = $state('');
  let roleFilter = $state('all');
  let sortBy = $state('name_asc');
  let page = $state(1);
  let perPage = $state(10);
  let menuFor = $state<number | null>(null);
  let menuStyle = $state('');

  async function load() {
    data = await teamUi.get();
  }
  load();

  const filtered = $derived.by(() => {
    const q = search.trim().toLowerCase();
    return (data?.members ?? [])
      .filter((m) => {
        const matchesSearch = !q || [m.name, m.email, m.role].some((v) => String(v || '').toLowerCase().includes(q));
        return matchesSearch && (roleFilter === 'all' || m.role === roleFilter);
      })
      .sort((l, r) => {
        if (sortBy === 'name_desc') return r.name.localeCompare(l.name);
        if (sortBy === 'email_asc') return l.email.localeCompare(r.email);
        if (sortBy === 'role') return l.role.localeCompare(r.role);
        return l.name.localeCompare(r.name);
      });
  });
  const lastPage = $derived(Math.max(1, Math.ceil(filtered.length / perPage)));
  $effect.pre(() => {
    if (page > lastPage) page = lastPage;
  });
  const visible = $derived(filtered.slice((page - 1) * perPage, page * perPage));
  const manage = $derived(!!data?.can_manage_members);
  const withoutTwoFactor = $derived((data?.members ?? []).filter((m) => m.two_factor === false).length);
  const total = $derived(data?.members.length ?? 0);

  function openMenu(e: MouseEvent, id: number) {
    if (menuFor === id) {
      menuFor = null;
      return;
    }
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    // x-anchor.bottom-end.fixed.offset.4: right-aligned below the trigger (w-36 = 9rem).
    menuStyle = `position: fixed; top: ${r.bottom + 4}px; left: ${r.right - 144}px;`;
    menuFor = id;
  }

  async function act(m: M, action: Role | 'remove') {
    menuFor = null;
    try {
      if (action === 'remove') await teamUi.remove(m.id);
      else await teamUi.setRole(m.id, action);
      await load();
    } catch (e) {
      notify.error(e instanceof ApiError ? e.message : String(e));
    }
  }

  const gridClass = $derived(`team-members-table-grid ${manage ? 'team-members-table-grid-2fa' : ''}`);
</script>

<svelte:window
  onclick={(e) => menuFor !== null && !(e.target as HTMLElement).closest('[data-member-menu]') && (menuFor = null)}
  onkeydown={(e) => e.key === 'Escape' && (menuFor = null)}
/>

<div>
  <TeamLayout>
    <div class="application-settings-form flex flex-col gap-6">
      <SettingsSection title="Members" flush>
        <div class="flex flex-col gap-2 border-b border-neutral-200 p-3 sm:flex-row sm:items-center sm:justify-between dark:border-white/[0.08]">
          <div class="relative w-full max-w-sm">
            <Reicon
              name="search"
              class="pointer-events-none absolute top-1/2 left-2.5 z-10 size-3.5 -translate-y-1/2 text-neutral-400 dark:text-fg-faint"
            />
            <input
              bind:value={search}
              oninput={() => (page = 1)}
              type="search"
              placeholder="Search members"
              aria-label="Search members"
              class="h-8! w-full rounded-lg! border-neutral-200! bg-white! py-0! pr-8! pl-8! text-[12px]! shadow-none! placeholder:text-neutral-400 focus:border-accent! focus:ring-0! dark:border-white/[0.08]! dark:bg-white/[0.035]! dark:text-fg! dark:placeholder:text-fg-faint"
            />
            {#if search}
              <button
                onclick={() => ((search = ''), (page = 1))}
                type="button"
                class="absolute top-1/2 right-2 flex size-5 -translate-y-1/2 items-center justify-center rounded text-neutral-400 transition-colors hover:bg-neutral-100 hover:text-black dark:text-fg-faint dark:hover:bg-white/[0.07] dark:hover:text-fg"
                aria-label="Clear search"
              >
                <Reicon name="x" class="size-3" />
              </button>
            {/if}
          </div>
          <div class="flex w-full gap-2 sm:w-auto">
            <div class="w-full sm:w-36">
              <Listbox
                id="member-role-filter"
                bind:value={roleFilter}
                onchange={() => (page = 1)}
                options={[
                  { value: 'all', label: 'All roles' },
                  { value: 'owner', label: 'Owner' },
                  { value: 'admin', label: 'Admin' },
                  { value: 'member', label: 'Member' },
                ]}
              />
            </div>
            <div class="w-full sm:w-40">
              <Listbox
                id="member-sort"
                bind:value={sortBy}
                onchange={() => (page = 1)}
                options={[
                  { value: 'name_asc', label: 'Name A–Z' },
                  { value: 'name_desc', label: 'Name Z–A' },
                  { value: 'email_asc', label: 'Email A–Z' },
                  { value: 'role', label: 'Role' },
                ]}
              />
            </div>
          </div>
        </div>
        {#if manage && data}
          <div class="border-b border-neutral-200 px-4 py-2.5 text-[12px] dark:border-white/[0.08]">
            {#if withoutTwoFactor > 0}
              <span class="text-warning-700 dark:text-warning"
                >{withoutTwoFactor} of {total}
                {total === 1 ? 'member' : 'members'}
                {withoutTwoFactor === 1 ? 'does' : 'do'} not have two-factor authentication enabled.</span
              >
            {:else}
              <span class="text-neutral-500 dark:text-fg-dim">All members have two-factor authentication enabled.</span>
            {/if}
          </div>
        {/if}
        {#if data && filtered.length > 0}
          <div class="data-table flex flex-col">
            <div class="data-table-header {gridClass}">
              <span>Name</span>
              <span>Email</span>
              <span>Role</span>
              {#if manage}<span>2FA</span>{/if}
              <span class="text-right">Actions</span>
            </div>
            {#each visible as m (m.id)}
              <div class="data-table-row {gridClass} border-b border-neutral-200 last:border-b-0 dark:border-white/[0.07]">
                <div>
                  <div class="flex items-center gap-2">
                    <div
                      class="flex size-7 shrink-0 items-center justify-center rounded-lg bg-neutral-100 text-[11px] font-semibold text-neutral-600 dark:bg-white/[0.06] dark:text-fg-dim"
                    >
                      {(m.name || m.email).slice(0, 1).toUpperCase()}
                    </div>
                    <span class="truncate text-[13px] font-medium text-black dark:text-fg">{m.name}</span>
                    {#if m.id === data.me}
                      <span
                        class="rounded-full bg-coollabs/10 px-1.5 py-0.5 text-[10px] font-medium text-coollabs dark:bg-brand/15 dark:text-brand-light"
                      >
                        You
                      </span>
                    {/if}
                  </div>
                </div>
                <div class="truncate text-[12px] text-neutral-500 dark:text-fg-dim">{m.email}</div>
                <div>
                  <span
                    class="inline-flex rounded-full bg-neutral-100 px-2 py-0.5 text-[10px] font-medium capitalize text-neutral-600 dark:bg-white/[0.06] dark:text-fg-dim"
                  >
                    {m.role}
                  </span>
                </div>
                {#if manage}
                  <div class="flex items-center">
                    <span
                      title={m.two_factor
                        ? 'Two-factor authentication is enabled for this account.'
                        : 'This account is not protected by two-factor authentication.'}
                      class="inline-flex items-center {m.two_factor ? 'text-green-600 dark:text-green-400' : 'text-neutral-400 dark:text-fg-faint'}"
                    >
                      {#if m.two_factor}
                        <svg class="size-4 shrink-0" xmlns="http://www.w3.org/2000/svg" fill="none" viewBox="0 0 24 24" stroke-width="1.5" stroke="currentColor" aria-hidden="true">
                          <path
                            stroke-linecap="round"
                            stroke-linejoin="round"
                            d="M9 12.75 11.25 15 15 9.75m-3-7.036A11.959 11.959 0 0 1 3.598 6 11.99 11.99 0 0 0 3 9.749c0 5.592 3.824 10.29 9 11.622 5.176-1.332 9-6.03 9-11.622 0-1.31-.21-2.571-.598-3.751h-.152c-3.196 0-6.1-1.248-8.25-3.285Z"
                          />
                        </svg>
                      {:else}
                        <Reicon name="x" class="size-4 shrink-0" />
                      {/if}
                      <span class="sr-only">Two-factor authentication is {m.two_factor ? 'enabled' : 'disabled'}</span>
                    </span>
                  </div>
                {/if}
                <div class="flex justify-end">
                  {#if manage && m.id !== data.me}
                    <div data-member-menu>
                      <button
                        type="button"
                        class="button h-7! px-2.5! text-[11px]!"
                        onclick={(e) => openMenu(e, m.id)}
                        aria-haspopup="menu"
                        aria-expanded={menuFor === m.id}
                      >
                        Manage
                      </button>
                      {#if menuFor === m.id}
                        <div role="menu" style={menuStyle} class="listbox-panel w-36! min-w-0! z-50!">
                          {#if data.my_role === 'owner'}
                            {#if m.role !== 'owner'}
                              <button type="button" class="listbox-option justify-start!" onclick={() => act(m, 'owner')}>Make owner</button>
                            {/if}
                            {#if m.role !== 'admin'}
                              <button type="button" class="listbox-option justify-start!" onclick={() => act(m, 'admin')}>Make admin</button>
                            {/if}
                            {#if m.role !== 'member'}
                              <button type="button" class="listbox-option justify-start!" onclick={() => act(m, 'member')}>Make member</button>
                            {/if}
                          {:else if data.my_role === 'admin'}
                            {#if m.role === 'admin'}
                              <button type="button" class="listbox-option justify-start!" onclick={() => act(m, 'member')}>Make member</button>
                            {:else if m.role === 'member'}
                              <button type="button" class="listbox-option justify-start!" onclick={() => act(m, 'admin')}>Make admin</button>
                            {/if}
                          {/if}
                          <div class="my-1 border-t border-neutral-200 dark:border-white/[0.08]"></div>
                          <button type="button" class="listbox-option justify-start! text-error! hover:text-error!" onclick={() => act(m, 'remove')}>
                            Remove member
                          </button>
                        </div>
                      {/if}
                    </div>
                  {/if}
                </div>
              </div>
            {/each}
          </div>
        {:else if data}
          <div>
            <Empty size="sm" title="No matching members" description="Try a different name, email address, or role." />
          </div>
        {/if}
        {#if filtered.length > 0}
          <Pager total={filtered.length} bind:page bind:pageSize={perPage} storageKey="coolify.page-size.team-members" />
        {/if}
      </SettingsSection>
    </div>
  </TeamLayout>
</div>
