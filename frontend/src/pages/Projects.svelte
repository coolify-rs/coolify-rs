<script lang="ts">
  import Icon from '../components/Icon.svelte';
  import ProjectForm from '../components/ProjectForm.svelte';
  import { api, type ProjectSummary } from '../lib/api';
  import { router } from '../lib/router.svelte';
  import { session } from '../lib/session.svelte';
  import { notify } from '../lib/toast.svelte';

  let projects = $state<ProjectSummary[] | null>(null);
  let error = $state('');
  let creating = $state(false);

  async function load() {
    try {
      projects = await api.get<ProjectSummary[]>('/projects');
    } catch (e) {
      error = String(e instanceof Error ? e.message : e);
    }
  }

  async function create(v: { name: string; description: string }) {
    const body: Record<string, string> = { name: v.name };
    if (v.description.trim()) body.description = v.description;
    const { uuid } = await api.post<{ uuid: string }>('/projects', body);
    notify.ok('Project created', v.name);
    router.go(`/projects/${uuid}`);
  }

  $effect(() => {
    session.info?.current_team?.id;
    load();
  });
</script>

<div class="page-head">
  <div>
    <h1>Projects</h1>
    <div class="summary">Projects group environments and the resources deployed in them.</div>
  </div>
  {#if session.isAdmin}
    <button class="button button-primary" onclick={() => (creating = true)}><Icon name="plus" /> New project</button>
  {/if}
</div>

<div class="card">
  <div class="card-well is-flush">
    {#if error}
      <div class="empty"><div class="empty-title">Could not load projects</div><div class="empty-text">{error}</div></div>
    {:else if projects === null}
      <div class="empty"><div class="empty-text">Loading…</div></div>
    {:else if projects.length === 0}
      <div class="empty">
        <div class="empty-title">No projects yet</div>
        <div class="empty-text">
          {session.isAdmin ? 'Create a project to start organizing your environments.' : 'An admin of this team can create the first project.'}
        </div>
      </div>
    {:else}
      <table class="table">
        <thead><tr><th>Name</th><th>Description</th><th></th></tr></thead>
        <tbody>
          {#each projects as p (p.uuid)}
            <tr class="clickable" onclick={() => router.go(`/projects/${p.uuid}`)}>
              <td class="cell-strong">{p.name}</td>
              <td class="cell-faint truncate">{p.description || 'No description'}</td>
              <td class="actions cell-faint"><Icon name="chevron" size={12} /></td>
            </tr>
          {/each}
        </tbody>
      </table>
    {/if}
  </div>
</div>

{#if creating}
  <ProjectForm title="New project" submitLabel="Create project" onsubmit={create} onclose={() => (creating = false)} />
{/if}
