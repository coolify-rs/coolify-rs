<script lang="ts">
  import { ApiError, api, type Member, type Team } from '../lib/api';
  import { session } from '../lib/session.svelte';
  import { notify } from '../lib/toast.svelte';

  let team = $state<Team | null>(null);
  let members = $state<Member[] | null>(null);
  let error = $state('');
  let saving = $state(false);

  async function load() {
    try {
      [team, members] = await Promise.all([api.get<Team>('/team'), api.get<Member[]>('/team/members')]);
      error = '';
    } catch (e) {
      error = e instanceof ApiError ? e.message : String(e);
    }
  }

  $effect(() => {
    session.info?.current_team?.id;
    load();
  });

  async function toggleFallback() {
    if (!team) return;
    saving = true;
    try {
      team = await api.patch<Team>('/team', { is_build_server_fallback_enabled: !team.is_build_server_fallback_enabled });
      notify.ok('Team updated');
    } catch (e) {
      notify.error('Could not update the team', e instanceof Error ? e.message : String(e));
    } finally {
      saving = false;
    }
  }
</script>

<div class="page-head">
  <div>
    <h1>{team?.name ?? 'Team'}</h1>
    <div class="summary">{team?.description || 'Members and settings of the current team.'}</div>
  </div>
</div>

{#if error}
  <div class="notice notice-error">{error}</div>
{:else if team}
  <div class="stack">
    <div class="card">
      <div class="card-header">
        <div>
          <div class="card-title">Members</div>
          <div class="card-description">Inviting and removing members is not available in coolify-rs yet.</div>
        </div>
      </div>
      <div class="card-well is-flush">
        <table class="table">
          <thead><tr><th>Name</th><th>Email</th><th>Two-factor</th><th>Joined</th></tr></thead>
          <tbody>
            {#each members ?? [] as m (m.id)}
              <tr>
                <td class="cell-strong">{m.name}{#if m.id === session.info?.user.id}<span class="cell-faint" style="margin-left:4px">(you)</span>{/if}</td>
                <td class="cell-faint">{m.email}</td>
                <td>
                  <span class="badge"><span class="badge-dot {m.two_factor_confirmed_at ? 'dot-success' : ''}"></span>
                    {m.two_factor_confirmed_at ? 'Enabled' : 'Off'}</span>
                </td>
                <td class="cell-faint">{new Date(m.created_at).toLocaleDateString()}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
    </div>

    <div class="card">
      <div class="card-header">
        <div>
          <div class="card-title">Build server fallback</div>
          <div class="card-description">Build on a deployment server when no build server is usable.</div>
        </div>
        {#if session.isAdmin}
          <button class="button" onclick={toggleFallback} disabled={saving}>
            {team.is_build_server_fallback_enabled ? 'Disable' : 'Enable'}
          </button>
        {/if}
      </div>
      <div class="card-well">
        <span class="badge">
          <span class="badge-dot {team.is_build_server_fallback_enabled ? 'dot-success' : ''}"></span>
          {team.is_build_server_fallback_enabled ? 'Enabled' : 'Disabled'}
        </span>
        {#if team.is_two_factor_required}
          <span class="badge" style="margin-left:6px"><span class="badge-dot dot-warning"></span>Two-factor required</span>
        {/if}
      </div>
    </div>
  </div>
{:else}
  <div class="empty"><div class="empty-text">Loading…</div></div>
{/if}
