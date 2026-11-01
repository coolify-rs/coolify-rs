<script lang="ts">
  // Port of upstream invitation/accept.blade.php (Controller::{showInvitation, link}).
  import { untrack } from 'svelte';
  import AuthShell from '../components/AuthShell.svelte';
  import Reicon from '../components/Reicon.svelte';
  import { ApiError } from '../lib/api';
  import { teamsUi, type InvitationView } from '../lib/teamsApi';
  import { session } from '../lib/session.svelte';
  import { router } from '../lib/router.svelte';
  import ErrorPage from '../components/ErrorPage.svelte';
  import { setLoginError } from '../lib/session.svelte';

  // `uuid` for /invitations/{uuid}, `token` for /auth/link?token=...
  let { uuid, token }: { uuid?: string; token?: string } = $props();

  let view = $state<InvitationView | null>(null);
  let failure = $state<ApiError | null>(null);
  let busy = $state(false);
  const highlighted = { ishighlighted: '' };

  function expired() {
    setLoginError('Invitation has expired or been revoked.');
    session.refresh().then(() => router.go(session.info ? '/' : '/login', true));
  }

  untrack(async () => {
    try {
      view = token !== undefined ? await teamsUi.link(token) : await teamsUi.invitation(uuid ?? '');
    } catch (e) {
      if (token !== undefined) expired();
      else failure = e instanceof ApiError ? e : new ApiError(500, String(e));
    }
  });

  async function accept(e: SubmitEvent) {
    e.preventDefault();
    if (busy) return;
    busy = true;
    try {
      if (token !== undefined) {
        await teamsUi.acceptLink(token);
        await session.refresh();
        router.go('/', true);
      } else {
        await teamsUi.acceptInvitation(uuid ?? '');
        await session.refresh();
        router.go('/team', true);
      }
    } catch (err) {
      if (token !== undefined) expired();
      else failure = err instanceof ApiError ? err : new ApiError(500, String(err));
    } finally {
      busy = false;
    }
  }
</script>

{#if failure}
  <ErrorPage code={failure.status === 404 ? 404 : 400} description={failure.status === 404 ? undefined : failure.message} />
{:else if view}
  <AuthShell title="coolify-rs" description="Review your invitation to join a team.">
    <div class="flex flex-col gap-4">
      <div class="auth-guidance">
        <Reicon name="teams" class="mt-0.5 size-4 shrink-0" />
        <p>You have been invited to collaborate on coolify-rs.</p>
      </div>
      <dl class="divide-y divide-neutral-200 rounded-lg border border-neutral-200 text-sm dark:divide-white/10 dark:border-white/10">
        <div class="flex items-center justify-between gap-4 px-3 py-2.5">
          <dt class="text-neutral-500 dark:text-fg-dim">Team</dt>
          <dd class="min-w-0 truncate font-medium text-neutral-900 dark:text-white">{view.team}</dd>
        </div>
        <div class="flex items-center justify-between gap-4 px-3 py-2.5">
          <dt class="text-neutral-500 dark:text-fg-dim">Role</dt>
          <dd class="font-medium text-neutral-900 dark:text-white">{view.role.charAt(0).toUpperCase() + view.role.slice(1)}</dd>
        </div>
      </dl>
      {#if view.already_member}
        <div class="flex items-start gap-3 rounded-lg border px-3 py-2.5 text-sm border-warning/35 bg-warning/10 text-warning">
          <Reicon name="alert-triangle" class="mt-0.5 size-4 shrink-0" />
          <div class="min-w-0 flex-1 leading-5">You are already a member of this team. Dismiss the invitation to continue.</div>
        </div>
      {/if}
      <form onsubmit={accept}>
        <button class="button w-full justify-center" type="submit" disabled={busy} {...highlighted}>
          {view.already_member ? 'Dismiss invitation' : 'Accept invitation'}
        </button>
      </form>
    </div>
  </AuthShell>
{:else}
  <main class="h-full bg-neutral-50 dark:bg-base"></main>
{/if}
