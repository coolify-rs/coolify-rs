<script lang="ts">
  // Port of upstream livewire/team/admin-view.blade.php (App\Livewire\Team\AdminView).
  import Reicon from '../components/Reicon.svelte';
  import Empty from '../components/Empty.svelte';
  import Listbox from '../components/Listbox.svelte';
  import Pager from '../components/Pager.svelte';
  import SettingsSection from '../components/SettingsSection.svelte';
  import TeamLayout from '../components/TeamLayout.svelte';
  import ConfirmModal from '../components/ConfirmModal.svelte';
  import { ApiError } from '../lib/api';
  import { teamsUi, type AdminData } from '../lib/teamsApi';
  import { router } from '../lib/router.svelte';
  import { notify } from '../lib/toast.svelte';

  let data = $state<AdminData | null>(null);
  let search = $state('');
  let teamFilter = $state('all');
  let sort = $state('name_asc');
  let page = $state(1);
  let perPage = $state(10);
  let target = $state<AdminData['users'][number] | null>(null);
  let timer: ReturnType<typeof setTimeout> | undefined;

  async function load() {
    try {
      data = await teamsUi.admin({ search, team_filter: teamFilter, sort, per_page: perPage, page });
    } catch {
      router.go('/', true);
    }
  }

  $effect(() => {
    void [teamFilter, sort, page, perPage];
    load();
  });

  function onSearch() {
    clearTimeout(timer);
    timer = setTimeout(() => {
      if (page === 1) load();
      else page = 1;
    }, 300);
  }

  async function remove(password: string): Promise<string | void> {
    if (!target) return;
    try {
      await teamsUi.adminDelete(target.id, password);
      page = 1;
      await load();
    } catch (e) {
      if (e instanceof ApiError && e.field('password')) return e.field('password');
      notify.failure(e instanceof ApiError ? e.message : String(e));
    }
  }
</script>

<div>
  <TeamLayout>
    <div class="application-settings-form">
      <SettingsSection title="Instance users" flush>
        <form
          onsubmit={(e) => {
            e.preventDefault();
            page = 1;
            load();
          }}
          class="flex flex-col gap-2 border-b border-neutral-200 p-3 sm:flex-row sm:items-center sm:justify-between dark:border-white/[0.08]"
        >
          <div class="relative w-full max-w-sm">
            <Reicon name="search" class="pointer-events-none absolute top-1/2 left-2.5 z-10 size-3.5 -translate-y-1/2 text-neutral-400 dark:text-fg-faint" />
            <input
              bind:value={search}
              oninput={onSearch}
              type="search"
              placeholder="Search users"
              aria-label="Search users"
              class="h-8! w-full rounded-lg! border-neutral-200! bg-white! py-0! pr-8! pl-8! text-[12px]! shadow-none! placeholder:text-neutral-400 focus:border-accent! focus:ring-0! dark:border-white/[0.08]! dark:bg-white/[0.035]! dark:text-fg! dark:placeholder:text-fg-faint"
            />
            <button
              type="button"
              onclick={() => {
                search = '';
                page = 1;
                load();
              }}
              class="absolute top-1/2 right-2 flex size-5 -translate-y-1/2 items-center justify-center rounded text-neutral-400 transition-colors hover:bg-neutral-100 hover:text-black dark:text-fg-faint dark:hover:bg-white/[0.07] dark:hover:text-fg {search ? '' : 'hidden'}"
              aria-label="Clear search"
            >
              <Reicon name="x" class="size-3" />
            </button>
          </div>
          <div class="flex w-full gap-2 sm:w-auto">
            <div class="w-full sm:w-40">
              <Listbox
                id="teamFilter"
                bind:value={teamFilter}
                onchange={() => (page = 1)}
                options={[
                  { value: 'all', label: 'All users' },
                  { value: 'current', label: 'Current team' },
                  { value: 'outside', label: 'Outside team' },
                ]}
              />
            </div>
            <div class="w-full sm:w-40">
              <Listbox
                id="sort"
                bind:value={sort}
                onchange={() => (page = 1)}
                options={[
                  { value: 'name_asc', label: 'Name A–Z' },
                  { value: 'name_desc', label: 'Name Z–A' },
                  { value: 'email_asc', label: 'Email A–Z' },
                  { value: 'email_desc', label: 'Email Z–A' },
                ]}
              />
            </div>
          </div>
        </form>

        {#if data && data.users.length > 0}
          <div class="data-table transition-opacity">
            <div class="data-table-header admin-users-table-grid">
              <span>Name</span>
              <span>Email</span>
              <span class="text-right">Actions</span>
            </div>
            {#each data.users as user (user.id)}
              <div class="data-table-row admin-users-table-grid border-b border-neutral-200 last:border-b-0 dark:border-white/[0.07]">
                <div class="flex min-w-0 items-center gap-2">
                  <div class="flex size-7 shrink-0 items-center justify-center rounded-lg bg-neutral-100 text-[11px] font-semibold text-neutral-600 dark:bg-white/[0.06] dark:text-fg-dim">
                    {(user.name || user.email).slice(0, 1).toUpperCase()}
                  </div>
                  <span class="truncate text-[13px] font-medium text-black dark:text-fg">{user.name}</span>
                </div>
                <div class="truncate text-[12px] text-neutral-500 dark:text-fg-dim">{user.email}</div>
                <div class="flex justify-end">
                  <div class="relative h-auto max-w-full inline-flex w-auto">
                    <button
                      type="button"
                      class="icon-button shrink-0 text-red-500 hover:text-red-600 dark:text-red-400 dark:hover:text-red-300"
                      title="Delete user"
                      aria-label="Delete {user.name}"
                      onclick={() => (target = user)}
                    >
                      <Reicon name="trash" class="size-3.5" />
                    </button>
                  </div>
                </div>
              </div>
            {/each}
          </div>
          <Pager total={data.total} bind:page bind:pageSize={perPage} storageKey="coolify.page-size.team-admin" separator="–" class="min-h-14" />
        {:else if data}
          <Empty title="No users found" description="Try a different name or email address." iconName="teams" size="sm" />
        {/if}
      </SettingsSection>
    </div>
  </TeamLayout>
</div>

{#if target && data}
  <ConfirmModal
    title="Confirm User Deletion?"
    actions={['The selected user and their default team resources will be permanently deleted.']}
    confirmWithText={data.confirm.text}
    confirmWithPassword={data.confirm.password}
    confirmationText={target.name}
    confirmationLabel="Enter the user name to confirm deletion"
    shortConfirmationLabel="User name"
    onconfirm={remove}
    onclose={() => (target = null)}
  />
{/if}
