<script lang="ts">
  // Port of upstream livewire/team/member/index.blade.php with livewire/team/member.blade.php,
  // livewire/team/invite-link.blade.php and livewire/team/invitations.blade.php.
  import Reicon from '../components/Reicon.svelte';
  import Empty from '../components/Empty.svelte';
  import Listbox from '../components/Listbox.svelte';
  import Pager from '../components/Pager.svelte';
  import SettingsSection from '../components/SettingsSection.svelte';
  import TeamLayout from '../components/TeamLayout.svelte';
  import Callout from '../components/Callout.svelte';
  import FormInput from '../components/FormInput.svelte';
  import { session } from '../lib/session.svelte';
  import { teamsUi, type InvitationsData } from '../lib/teamsApi';
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

  let invitations = $state<InvitationsData | null>(null);
  let inviteEmail = $state('');
  let inviteRole = $state('member');
  let inviteBusy = $state(false);
  let inviteError = $state<ApiError | null>(null);
  let copiedId = $state<number | null>(null);

  async function load() {
    data = await teamUi.get();
    invitations = await teamsUi.invitations();
  }
  load();

  const roleOptions = $derived([
    ...(data?.my_role === 'owner' ? [{ value: 'owner', label: 'Owner' }] : []),
    { value: 'admin', label: 'Admin' },
    { value: 'member', label: 'Member' },
  ]);

  /** `InviteLink::viaEmail` and `viaLink`. */
  async function invite(via: 'email' | 'link') {
    if (inviteBusy) return;
    inviteBusy = true;
    inviteError = null;
    try {
      const r = await teamsUi.invite(inviteEmail, inviteRole, via);
      notify.success(r.message);
      // `$this->email = strtolower($this->email)` once validation passed.
      inviteEmail = inviteEmail.toLowerCase();
      invitations = await teamsUi.invitations();
    } catch (e) {
      const message = e instanceof ApiError ? e.message : String(e);
      if (e instanceof ApiError) inviteError = e;
      if (message.includes(inviteEmail.toLowerCase())) inviteEmail = inviteEmail.toLowerCase();
      notify.failure(message);
    } finally {
      inviteBusy = false;
    }
  }

  async function revoke(id: number) {
    try {
      const r = await teamsUi.revoke(id);
      notify.success(r.message);
    } catch (e) {
      notify.failure(e instanceof ApiError ? e.message : String(e));
    }
    invitations = await teamsUi.invitations();
  }

  async function copyLink(id: number, value: string) {
    try {
      await navigator.clipboard.writeText(value);
      copiedId = id;
      setTimeout(() => (copiedId = null), 1500);
    } catch {
      // Clipboard access can be denied.
    }
  }

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
      // `dispatch('reloadWindow')`.
      await Promise.all([load(), session.refresh()]);
    } catch (e) {
      notify.failure(e instanceof ApiError ? e.message : String(e));
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
                    {#if m.avatar_url}
                      <img src={m.avatar_url} alt={m.name} class="size-7 shrink-0 rounded-lg object-cover" />
                    {:else}
                      <div
                        class="flex size-7 shrink-0 items-center justify-center rounded-lg bg-neutral-100 text-[11px] font-semibold text-neutral-600 dark:bg-white/[0.06] dark:text-fg-dim"
                      >
                        {(m.name || m.email).slice(0, 1).toUpperCase()}
                      </div>
                    {/if}
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
                        <span class="text-[12px]" aria-hidden="true">Off</span>
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

      {#if invitations?.can_manage}
        <div>
          <form
            onsubmit={(e) => {
              e.preventDefault();
              invite('link');
            }}
          >
            <SettingsSection title="Invite a member" description="Create a reusable invitation link or deliver it by email.">
              {#snippet actions()}
                {#if invitations?.email_enabled}
                  <button type="button" class="button" onclick={() => invite('email')}>
                    <Reicon name="notifications" class="size-3.5" />
                    Send email
                  </button>
                {/if}
                <button type="submit" class="button button-highlighted">
                  <Reicon name="plus" class="size-3.5" />
                  Generate link
                </button>
              {/snippet}
              {#if !invitations?.email_enabled && invitations?.instance_admin}
                <Callout type="warning" title="Email delivery is not configured">
                  Configure transactional email in instance settings to send invitations directly.
                </Callout>
              {/if}
              <div class="mt-4 grid gap-4 lg:grid-cols-2">
                <FormInput id="email" type="email" label="Email address" placeholder="teammate@example.com" required bind:value={inviteEmail} error={inviteError?.field('email')} />
                <Listbox id="role" label="Role" bind:value={inviteRole} options={roleOptions} />
              </div>
            </SettingsSection>
          </form>
        </div>
        <div>
          {#if invitations.invitations.length > 0}
            <SettingsSection title="Pending invitations" description="Invitation links that have not been accepted yet." flush>
              <div class="overflow-x-auto">
                <div class="data-table min-w-[760px]">
                  <div class="data-table-header grid-cols-[minmax(13rem,1.2fr)_7rem_7rem_minmax(15rem,1.5fr)_6rem]">
                    <span>Email</span>
                    <span>Method</span>
                    <span>Role</span>
                    <span>Invitation link</span>
                    <span class="text-right">Actions</span>
                  </div>
                  {#each invitations.invitations as row (row.id)}
                    <div class="data-table-row grid-cols-[minmax(13rem,1.2fr)_7rem_7rem_minmax(15rem,1.5fr)_6rem] border-b border-neutral-200 last:border-b-0 dark:border-white/[0.07]">
                      <div class="truncate text-[12px] font-medium text-black dark:text-fg">{row.email}</div>
                      <div class="text-[12px] capitalize text-neutral-500 dark:text-fg-dim">{row.via}</div>
                      <div class="text-[12px] capitalize text-neutral-500 dark:text-fg-dim">{row.role}</div>
                      <div class="flex min-w-0 items-center gap-2">
                        <span class="min-w-0 truncate font-mono text-[12px] text-neutral-500 dark:text-fg-dim" title={row.link}>{row.link}</span>
                        <button type="button" title="Copy invitation link" aria-label="Copy invitation link" class="icon-button group shrink-0" onclick={() => copyLink(row.id, row.link)}>
                          <span class="inline-flex transition-transform duration-150 ease-out group-active:scale-75">
                            {#if copiedId === row.id}<Reicon name="check" class="size-3.5 text-success" />{:else}<Reicon name="copy" class="size-3.5" />{/if}
                          </span>
                        </button>
                      </div>
                      <div class="text-right">
                        <button type="button" class="button h-7! px-2.5! text-[11px]! text-error! hover:text-error!" onclick={() => revoke(row.id)}>Revoke</button>
                      </div>
                    </div>
                  {/each}
                </div>
              </div>
            </SettingsSection>
          {/if}
        </div>
      {/if}
    </div>
  </TeamLayout>
</div>
