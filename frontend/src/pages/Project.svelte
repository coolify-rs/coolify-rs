<script lang="ts">
  import Icon from '../components/Icon.svelte';
  import ProjectForm from '../components/ProjectForm.svelte';
  import ConfirmDialog from '../components/ConfirmDialog.svelte';
  import { ApiError, api, type Environment, type Project } from '../lib/api';
  import { link, router } from '../lib/router.svelte';
  import { session } from '../lib/session.svelte';
  import { notify } from '../lib/toast.svelte';

  let { uuid }: { uuid: string } = $props();

  let project = $state<Project | null>(null);
  let error = $state<ApiError | null>(null);
  let editing = $state(false);
  let deleting = $state(false);
  let creatingEnv = $state(false);
  let editingEnv = $state<Environment | null>(null);
  let deletingEnv = $state<Environment | null>(null);

  async function load() {
    try {
      project = await api.get<Project>(`/projects/${encodeURIComponent(uuid)}`);
      error = null;
    } catch (e) {
      error = e instanceof ApiError ? e : new ApiError(0, String(e));
    }
  }

  $effect(() => {
    uuid;
    load();
  });

  const base = $derived(`/projects/${encodeURIComponent(uuid)}`);

  async function update(v: { name: string; description: string }) {
    await api.patch(base, { name: v.name, description: v.description.trim() ? v.description : null });
    notify.ok('Project updated');
    await load();
  }

  async function remove() {
    await api.del(base);
    notify.ok('Project deleted', project?.name);
    router.go('/projects');
  }

  async function createEnv(v: { name: string }) {
    await api.post(`${base}/environments`, { name: v.name });
    notify.ok('Environment created', v.name);
    await load();
  }

  async function updateEnv(env: Environment, v: { name: string; description: string }) {
    const body: Record<string, string | null> = { description: v.description.trim() ? v.description : null };
    if (v.name !== env.name) body.name = v.name;
    await api.patch(`${base}/environments/${encodeURIComponent(env.uuid)}`, body);
    notify.ok('Environment updated');
    await load();
  }

  async function removeEnv(env: Environment) {
    await api.del(`${base}/environments/${encodeURIComponent(env.uuid)}`);
    notify.ok('Environment deleted', env.name);
    await load();
  }
</script>

{#if error}
  <div class="card"><div class="card-well"><div class="empty">
    <div class="empty-title">{error.status === 404 ? 'Project not found' : 'Could not load the project'}</div>
    <div class="empty-text">{error.message}</div>
    <a class="button" href="/projects" use:link>Back to projects</a>
  </div></div></div>
{:else if project}
  <div class="page-head">
    <div>
      <div class="summary"><a href="/projects" use:link>Projects</a></div>
      <h1>{project.name}</h1>
      <div class="summary">{project.description || 'No description'}</div>
    </div>
    {#if session.isAdmin}
      <div class="row">
        <button class="button" onclick={() => (editing = true)}>Edit</button>
        <button class="button button-danger" onclick={() => (deleting = true)}>Delete</button>
      </div>
    {/if}
  </div>

  <div class="stack">
    <div class="card">
      <div class="card-header">
        <div>
          <div class="card-title">Environments</div>
          <div class="card-description">Each project starts with a production environment.</div>
        </div>
        {#if session.isAdmin}
          <button class="button button-primary" onclick={() => (creatingEnv = true)}><Icon name="plus" /> New environment</button>
        {/if}
      </div>
      <div class="card-well is-flush">
        {#if project.environments.length === 0}
          <div class="empty"><div class="empty-title">No environments</div></div>
        {:else}
          <table class="table">
            <thead><tr><th>Name</th><th>Description</th><th class="actions"></th></tr></thead>
            <tbody>
              {#each project.environments as env (env.uuid)}
                <tr class="clickable" onclick={() => router.go(`${base}/environments/${encodeURIComponent(env.uuid)}`)}>
                  <td class="cell-strong">{env.name}</td>
                  <td class="cell-faint truncate">{env.description || 'No description'}</td>
                  <td class="actions">
                    {#if session.isAdmin}
                      <button class="button button-ghost" onclick={(e) => (e.stopPropagation(), (editingEnv = env))}>Edit</button>
                      <button class="button button-ghost button-danger" onclick={(e) => (e.stopPropagation(), (deletingEnv = env))}>Delete</button>
                    {/if}
                  </td>
                </tr>
              {/each}
            </tbody>
          </table>
        {/if}
      </div>
    </div>

    <div class="card">
      <div class="card-header"><div class="card-title">Details</div></div>
      <div class="card-well">
        <dl class="kv">
          <dt>UUID</dt><dd class="mono">{project.uuid}</dd>
          <dt>Created</dt><dd>{new Date(project.created_at).toLocaleString()}</dd>
          <dt>Updated</dt><dd>{new Date(project.updated_at).toLocaleString()}</dd>
        </dl>
      </div>
    </div>
  </div>

  {#if editing}
    <ProjectForm
      title="Edit project"
      submitLabel="Save"
      initial={{ name: project.name, description: project.description ?? '' }}
      onsubmit={update}
      onclose={() => (editing = false)}
    />
  {/if}
  {#if deleting}
    <ConfirmDialog
      title="Delete project"
      message={`Delete "${project.name}" and its environments? Projects with resources cannot be deleted.`}
      onconfirm={remove}
      onclose={() => (deleting = false)}
    />
  {/if}
  {#if creatingEnv}
    <ProjectForm title="New environment" submitLabel="Create environment" withDescription={false} onsubmit={createEnv} onclose={() => (creatingEnv = false)} />
  {/if}
  {#if editingEnv}
    {@const env = editingEnv}
    <ProjectForm
      title="Edit environment"
      submitLabel="Save"
      initial={{ name: env.name, description: env.description ?? '' }}
      onsubmit={(v) => updateEnv(env, v)}
      onclose={() => (editingEnv = null)}
    />
  {/if}
  {#if deletingEnv}
    {@const env = deletingEnv}
    <ConfirmDialog
      title="Delete environment"
      message={`Delete the "${env.name}" environment? Environments with resources cannot be deleted.`}
      onconfirm={() => removeEnv(env)}
      onclose={() => (deletingEnv = null)}
    />
  {/if}
{:else}
  <div class="empty"><div class="empty-text">Loading…</div></div>
{/if}
