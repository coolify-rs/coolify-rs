<script lang="ts">
  // Port of upstream livewire/admin/index.blade.php.
  import Callout from '../components/Callout.svelte';
  import Empty from '../components/Empty.svelte';
  import ErrorPage from '../components/ErrorPage.svelte';
  import FormInput from '../components/FormInput.svelte';
  import Reicon from '../components/Reicon.svelte';
  import StatusBadge from '../components/StatusBadge.svelte';
  import { ApiError, request } from '../lib/api';

  type Page = { name: string; email: string; active: number; inactive: number; impersonating: boolean };
  type Found = { id: number; name: string; email: string; active: boolean };

  document.title = 'Admin | coolify-rs';

  let data = $state<Page | null>(null);
  let forbidden = $state(false);
  let search = $state('');
  let searched = $state('');
  let found = $state<Found[]>([]);

  request<Page>('GET', '/ui/admin')
    .then((d) => (data = d))
    .catch((e) => {
      if (e instanceof ApiError && e.status === 403) forbidden = true;
      else throw e;
    });

  async function submitSearch(e: SubmitEvent) {
    e.preventDefault();
    if (search === '') {
      searched = '';
      return;
    }
    found = (await request<{ users: Found[] }>('POST', '/ui/admin/search', { search })).users;
    searched = search;
  }

  async function switchUser(id: number) {
    const r = await request<{ redirect: string }>('POST', '/ui/admin/switch', { user_id: id });
    window.location.assign(r.redirect);
  }

  async function back() {
    const r = await request<{ redirect?: string }>('POST', '/ui/admin/back', {});
    if (r.redirect) window.location.assign(r.redirect);
  }
</script>

{#if forbidden}
  <ErrorPage code={403} />
{:else if data}
  <div class="mt-8 flex w-full max-w-none flex-col gap-6 lg:mt-3">
    <section class="grid gap-3 sm:grid-cols-3">
      <div class="rounded-[10px] border border-neutral-200 bg-white p-4 dark:border-white/[0.07] dark:bg-surface">
        <div class="text-xs font-medium text-neutral-500 dark:text-fg-dim">Current user</div>
        <div class="mt-2 truncate text-sm font-semibold text-black dark:text-fg">{data.name}</div>
        <div class="mt-0.5 truncate text-xs text-neutral-500 dark:text-fg-faint">{data.email}</div>
      </div>
      <div class="rounded-[10px] border border-neutral-200 bg-white p-4 dark:border-white/[0.07] dark:bg-surface">
        <div class="text-xs font-medium text-neutral-500 dark:text-fg-dim">Active subscribers</div>
        <div class="mt-2 text-2xl font-semibold tracking-tight text-black dark:text-fg">{data.active}</div>
      </div>
      <div class="rounded-[10px] border border-neutral-200 bg-white p-4 dark:border-white/[0.07] dark:bg-surface">
        <div class="text-xs font-medium text-neutral-500 dark:text-fg-dim">Inactive subscribers</div>
        <div class="mt-2 text-2xl font-semibold tracking-tight text-black dark:text-fg">{data.inactive}</div>
      </div>
    </section>

    {#if data.impersonating}
      <Callout type="warning" title="Impersonation is active">
        <div class="flex flex-wrap items-center justify-between gap-3">
          <span>You are viewing coolify-rs as {data.name}.</span>
          <button type="button" class="button" onclick={back}>Return to root user</button>
        </div>
      </Callout>
    {/if}

    <section class="application-settings-section">
      <div class="application-settings-section-header">
        <div>
          <h2>User lookup</h2>
          <p>Find an account and switch into it for support or administration.</p>
        </div>
      </div>
      <div class="application-settings-section-body p-0!">
        <form onsubmit={submitSearch} class="flex items-end gap-2 border-b border-neutral-200 p-4 dark:border-white/[0.06]">
          <div class="max-w-md flex-1">
            <FormInput id="search" label="Name or email" placeholder="Search for a user…" bind:value={search} />
          </div>
          <button type="submit" class="button">
            <Reicon name="search" class="size-4" />
            Search
          </button>
        </form>

        {#if searched}
          {#if found.length === 0}
            <Empty size="sm" title="No users found" description="No account matches {searched}." iconName="profile" />
          {:else}
            <div class="data-table">
              <div class="data-table-header admin-search-table-grid">
                <span>Name</span>
                <span>Email</span>
                <span>Subscription</span>
                <span class="text-right">Actions</span>
              </div>
              {#each found as user (user.id)}
                <div class="data-table-row admin-search-table-grid border-b border-neutral-200 last:border-b-0 dark:border-white/[0.06]">
                  <div class="truncate text-[12px] font-semibold text-black dark:text-fg">{user.name}</div>
                  <div class="truncate text-[11px] text-neutral-600 dark:text-fg-dim">{user.email}</div>
                  <div>
                    <StatusBadge label={user.active ? 'Active' : 'Inactive'} type={user.active ? 'success' : 'neutral'} />
                  </div>
                  <div class="flex justify-end">
                    <button type="button" class="button" onclick={() => switchUser(user.id)}>Switch user</button>
                  </div>
                </div>
              {/each}
              <div class="flex min-h-11 items-center border-t border-neutral-200 px-4 text-[11px] text-neutral-500 dark:border-white/[0.06] dark:text-fg-dim">
                {found.length} {found.length === 1 ? 'matching user' : 'matching users'}
              </div>
            </div>
          {/if}
        {:else}
          <Empty size="sm" title="Search for an account" description="Enter a name or email address to begin." iconName="profile" />
        {/if}
      </div>
    </section>
  </div>
{/if}
