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
  import Dropdown from '../components/Dropdown.svelte';
  import ConfirmModal from '../components/ConfirmModal.svelte';
  import TableSortHeader from '../components/TableSortHeader.svelte';
  import MemberAccessEditor from '../components/MemberAccessEditor.svelte';
  import { session } from '../lib/session.svelte';
  import { teamsUi, type AccessRow, type InvitationsData, type InvitationRow, type StoredGrant } from '../lib/teamsApi';
  import { teamUi, ApiError, type Role, type TeamPageData } from '../lib/api';
  import { notify } from '../lib/toast.svelte';

  type M = TeamPageData['members'][number];

  let data = $state<TeamPageData | null>(null);
  let search = $state('');
  let roleFilter = $state('all');
  let sortBy = $state('name-asc');
  let page = $state(1);
  let perPage = $state(10);
  let removing = $state<M | null>(null);
  let revokingInvite = $state<InvitationRow | null>(null);
  let inviteSort = $state<string | null>(null);
  /** The access editor (`livewire:team.member.access`). */
  let editor = $state<{ title: string; summary: string | null; rows: AccessRow[]; memberId: number | null; forInvitation: boolean } | null>(null);
  /** `InviteLink::$grants`: `defaultMemberGrants()` until the editor sets others. */
  let inviteGrants = $state<StoredGrant[]>([{ scope: 'team', uuid: null, levels: ['read'] }]);
  let inviteSummary = $state('Team: read');

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
      const r = await teamsUi.invite(inviteEmail, inviteRole, via, inviteGrants);
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

  const roleRank = (role: string) => ({ member: 1, admin: 2, owner: 3 })[role] ?? 0;
  const filtered = $derived.by(() => {
    const q = search.trim().toLowerCase();
    const [column, direction] = sortBy.split('-');
    const order = direction === 'desc' ? -1 : 1;
    const byName = (l: M, r: M) => String(l.name || '').localeCompare(String(r.name || ''));
    const compare =
      {
        name: byName,
        email: (l: M, r: M) => String(l.email || '').localeCompare(String(r.email || '')),
        role: (l: M, r: M) => roleRank(l.role) - roleRank(r.role),
        twofa: (l: M, r: M) => Number(l.two_factor) - Number(r.two_factor),
      }[column] ?? byName;
    return (data?.members ?? [])
      .filter((m) => {
        const matchesSearch = !q || [m.name, m.email, m.role].some((v) => String(v || '').toLowerCase().includes(q));
        return matchesSearch && (roleFilter === 'all' || m.role === roleFilter);
      })
      .sort((l, r) => order * compare(l, r) || byName(l, r));
  });
  const lastPage = $derived(Math.max(1, Math.ceil(filtered.length / perPage)));
  $effect.pre(() => {
    if (page > lastPage) page = lastPage;
  });
  const visible = $derived(filtered.slice((page - 1) * perPage, page * perPage));
  const manage = $derived(!!data?.can_manage_members);
  const withoutTwoFactor = $derived((data?.members ?? []).filter((m) => m.two_factor === false).length);
  const total = $derived(data?.members.length ?? 0);

  async function act(m: M, action: Role | 'remove') {
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

  /** `open-member-access`. */
  async function editAccess(m: M) {
    try {
      const r = await teamsUi.memberAccess(m.id);
      editor = { title: r.title, summary: r.summary, rows: r.rows, memberId: m.id, forInvitation: false };
    } catch (e) {
      notify.failure(e instanceof ApiError ? e.message : String(e));
    }
  }

  /** `open-invitation-access`: the editor starts from the invite form's grants. */
  function editInvitationAccess() {
    const groups = new Map<string, AccessRow>();
    const names: Record<string, string> = { read: 'Read', 'read:sensitive': 'ReadSensitive', deploy: 'Deploy', write: 'Write', delete: 'Delete', root: 'Root' };
    for (const g of inviteGrants) {
      const levels = g.levels.map((l) => names[l]).filter(Boolean);
      const k = levels.join(',');
      const target = g.scope === 'team' ? 'team' : `${g.scope}:${g.uuid}`;
      const row = groups.get(k) ?? { key: Math.random().toString(36).slice(2, 12), targets: [], levels: Object.fromEntries(Object.values(names).map((n) => [n, levels.includes(n)])) };
      if (!row.targets.includes(target)) row.targets.push(target);
      groups.set(k, row);
    }
    editor = { title: 'Access for the invited member', summary: null, rows: [...groups.values()], memberId: null, forInvitation: true };
  }

  /** `Invitations::setSort`. */
  const invitationRows = $derived.by(() => {
    const list = [...(invitations?.invitations ?? [])];
    if (inviteSort === null) return list;
    const [column, direction] = inviteSort.split('-');
    const order = direction === 'desc' ? -1 : 1;
    const value = (i: InvitationRow): number | string =>
      column === 'role' ? roleRank(i.role) : String((i as unknown as Record<string, unknown>)[column] ?? '').toLowerCase();
    return list.sort((l, r) => {
      const a = value(l);
      const b = value(r);
      return order * (a < b ? -1 : a > b ? 1 : 0) || l.email.toLowerCase().localeCompare(r.email.toLowerCase());
    });
  });
</script>

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
            <div class="w-full sm:w-40 min-[901px]:hidden">
              <Listbox
                id="member-sort"
                bind:value={sortBy}
                onchange={() => (page = 1)}
                options={[
                  { value: 'name-asc', label: 'Name A–Z' },
                  { value: 'name-desc', label: 'Name Z–A' },
                  { value: 'email-asc', label: 'Email A–Z' },
                  { value: 'email-desc', label: 'Email Z–A' },
                  { value: 'role-desc', label: 'Role' },
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
              <TableSortHeader label="Name" asc="name-asc" desc="name-desc" bind:value={sortBy} onsort={() => (page = 1)} />
              <TableSortHeader label="Email" asc="email-asc" desc="email-desc" bind:value={sortBy} onsort={() => (page = 1)} />
              <TableSortHeader label="Role" asc="role-asc" desc="role-desc" first="desc" bind:value={sortBy} onsort={() => (page = 1)} />
              {#if manage}<TableSortHeader label="2FA" asc="twofa-asc" desc="twofa-desc" bind:value={sortBy} onsort={() => (page = 1)} />{/if}
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
                <div class="min-w-0">
                  <div class="truncate text-[12px] text-neutral-500 dark:text-fg-dim">{m.email}</div>
                  {#if manage && m.role === 'member' && m.access_summary}
                    <div class="mt-0.5 truncate text-[11px] text-neutral-400 dark:text-fg-faint" title="Access: {m.access_summary}">
                      Access: {m.access_summary}
                    </div>
                  {/if}
                </div>
                <div>
                  <button
                    type="button"
                    class="table-filter-value table-filter-badge inline-flex rounded-full border border-transparent bg-neutral-100 px-2 py-0.5 text-[10px] font-medium capitalize text-neutral-600 dark:bg-white/[0.06] dark:text-fg-dim"
                    title="Filter by {m.role}"
                    onclick={() => ((roleFilter = roleFilter === m.role ? 'all' : m.role), (page = 1))}
                  >
                    {m.role}
                  </button>
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
                    {#if m.role === 'owner' && data.my_role !== 'owner'}
                      <button
                        type="button"
                        class="icon-button shrink-0 text-red-500 hover:text-red-600 dark:text-red-400 dark:hover:text-red-300"
                        title="Remove member"
                        aria-label="Remove {m.name}"
                        onclick={() => (removing = m)}
                      >
                        <Reicon name="trash" class="size-3.5" />
                      </button>
                    {:else}
                      <Dropdown panelClass="w-44!" role="menu">
                        {#snippet trigger({ open })}
                          <button type="button" class="icon-button" title="Member actions" aria-label="Member actions" aria-haspopup="menu" aria-expanded={open}>
                            <Reicon name="dots-horizontal" class="size-4" />
                          </button>
                        {/snippet}
                        {#snippet children({ close })}
                          {#if data?.my_role === 'owner'}
                            {#if m.role !== 'owner'}
                              <button type="button" role="menuitem" class="listbox-option justify-start!" onclick={() => (close(), act(m, 'owner'))}>Make owner</button>
                            {/if}
                            {#if m.role !== 'admin'}
                              <button type="button" role="menuitem" class="listbox-option justify-start!" onclick={() => (close(), act(m, 'admin'))}>Make admin</button>
                            {/if}
                            {#if m.role !== 'member'}
                              <button type="button" role="menuitem" class="listbox-option justify-start!" onclick={() => (close(), act(m, 'member'))}>Make member</button>
                            {/if}
                          {:else if data?.my_role === 'admin'}
                            {#if m.role === 'admin'}
                              <button type="button" role="menuitem" class="listbox-option justify-start!" onclick={() => (close(), act(m, 'member'))}>Make member</button>
                            {:else if m.role === 'member'}
                              <button type="button" role="menuitem" class="listbox-option justify-start!" onclick={() => (close(), act(m, 'admin'))}>Make admin</button>
                            {/if}
                          {/if}
                          {#if m.role === 'member'}
                            <button type="button" role="menuitem" class="listbox-option justify-start!" onclick={() => (close(), editAccess(m))}>Edit access</button>
                          {/if}
                          <div class="my-1 border-t border-neutral-200 dark:border-white/[0.08]"></div>
                          <div class="row-actions-danger contents">
                            <button type="button" role="menuitem" class="listbox-option justify-start!" onclick={() => (close(), (removing = m))}>Remove member</button>
                          </div>
                        {/snippet}
                      </Dropdown>
                    {/if}
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
                <div class="flex items-end gap-2">
                  <div class="min-w-0 flex-1">
                    <Listbox id="role" label="Role" bind:value={inviteRole} options={roleOptions} />
                  </div>
                  {#if inviteRole === 'member'}
                    <div class="shrink-0">
                      <button type="button" class="button button-highlighted" title={inviteSummary} onclick={editInvitationAccess}>
                        <Reicon name="keys" class="size-3.5" />
                        Scopes
                      </button>
                    </div>
                  {/if}
                </div>
                {#if inviteRole === 'member'}
                  <p class="text-[12px] text-neutral-500 lg:col-span-2 dark:text-fg-faint">
                    Scopes: <span class="text-neutral-700 dark:text-fg-dim">{inviteSummary}</span>
                  </p>
                {/if}
              </div>
            </SettingsSection>
          </form>
        </div>
        <div>
          {#if invitations.invitations.length > 0}
            <SettingsSection title="Pending invitations" description="Invitation links that have not been accepted yet." flush>
              <div class="overflow-x-auto">
                <div class="data-table min-w-[600px] transition-opacity">
                  <div class="data-table-header grid-cols-[minmax(13rem,1fr)_7rem_10rem_4rem_4.5rem]">
                    <TableSortHeader label="Email" asc="email-asc" desc="email-desc" bind:value={inviteSort} />
                    <TableSortHeader label="Method" asc="via-asc" desc="via-desc" bind:value={inviteSort} />
                    <TableSortHeader label="Role" asc="role-asc" desc="role-desc" first="desc" bind:value={inviteSort} />
                    <span>Link</span>
                    <span class="text-right">Actions</span>
                  </div>
                  {#each invitationRows as row (row.id)}
                    <div class="data-table-row grid-cols-[minmax(13rem,1fr)_7rem_10rem_4rem_4.5rem] border-b border-neutral-200 last:border-b-0 dark:border-white/[0.07]">
                      <div class="truncate text-[12px] font-medium text-black dark:text-fg">{row.email}</div>
                      <div class="text-[12px] capitalize text-neutral-500 dark:text-fg-dim">{row.via}</div>
                      <div class="min-w-0 text-[12px] text-neutral-500 dark:text-fg-dim">
                        <div class="capitalize">{row.role}</div>
                        {#if row.role === 'member' && row.access_summary}
                          <div class="truncate text-[11px] text-neutral-400 dark:text-fg-faint" title={row.access_summary}>{row.access_summary}</div>
                        {/if}
                      </div>
                      <div class="flex items-center">
                        <button type="button" title="Copy invitation link" aria-label="Copy invitation link" class="icon-button group shrink-0" onclick={() => copyLink(row.id, row.link)}>
                          <span class="inline-flex transition-transform duration-150 ease-out group-active:scale-75">
                            {#if copiedId === row.id}<Reicon name="check" class="size-3.5 text-success" />{:else}<Reicon name="copy" class="size-3.5" />{/if}
                          </span>
                        </button>
                      </div>
                      <div class="flex justify-end">
                        <button
                          type="button"
                          class="icon-button shrink-0 text-red-500 hover:text-red-600 dark:text-red-400 dark:hover:text-red-300"
                          title="Revoke invitation"
                          aria-label="Revoke invitation for {row.email}"
                          onclick={() => (revokingInvite = row)}
                        >
                          <Reicon name="trash" class="size-3.5" />
                        </button>
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

{#if removing}
  <ConfirmModal
    title="Remove team member?"
    actions={['This user loses access to all resources of this team.']}
    confirmWithText={false}
    step2ButtonText="Remove member"
    onconfirm={async () => {
      const m = removing;
      removing = null;
      if (m) await act(m, 'remove');
    }}
    onclose={() => (removing = null)}
  />
{/if}

{#if revokingInvite}
  <ConfirmModal
    title="Revoke invitation?"
    actions={[`The invitation link for ${revokingInvite.email} stops working.`]}
    confirmWithText={false}
    step2ButtonText="Revoke invitation"
    onconfirm={async () => {
      const i = revokingInvite;
      revokingInvite = null;
      if (i) await revoke(i.id);
    }}
    onclose={() => (revokingInvite = null)}
  />
{/if}

{#if editor}
  <MemberAccessEditor
    title={editor.title}
    summary={editor.summary}
    rows={editor.rows}
    memberId={editor.memberId}
    forInvitation={editor.forInvitation}
    onclose={() => (editor = null)}
    onsaved={() => load()}
    onchosen={(access, summary) => ((inviteGrants = access), (inviteSummary = summary))}
  />
{/if}
