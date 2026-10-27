<script lang="ts">
  import { ApiError, api, type Environment } from '../lib/api';
  import { link } from '../lib/router.svelte';

  let { uuid, env }: { uuid: string; env: string } = $props();

  type Details = Environment & Record<string, unknown>;
  let details = $state<Details | null>(null);
  let error = $state<ApiError | null>(null);

  const resourceKinds = [
    ['applications', 'Applications'],
    ['services', 'Services'],
    ['postgresqls', 'PostgreSQL'],
    ['mysqls', 'MySQL'],
    ['mariadbs', 'MariaDB'],
    ['mongodbs', 'MongoDB'],
    ['redis', 'Redis'],
  ] as const;

  $effect(() => {
    const path = `/projects/${encodeURIComponent(uuid)}/${encodeURIComponent(env)}`;
    api
      .get<Details>(path)
      .then((d) => ((details = d), (error = null)))
      .catch((e) => (error = e instanceof ApiError ? e : new ApiError(0, String(e))));
  });
</script>

<div class="page-head">
  <div>
    <div class="summary">
      <a href="/projects" use:link>Projects</a> /
      <a href={`/projects/${encodeURIComponent(uuid)}`} use:link>Project</a>
    </div>
    <h1>{details?.name ?? 'Environment'}</h1>
    {#if details}<div class="summary">{details.description || 'No description'}</div>{/if}
  </div>
</div>

{#if error}
  <div class="card"><div class="card-well"><div class="empty">
    {#if error.status === 501}
      <div class="empty-title">Resources are not available in coolify-rs yet</div>
      <div class="empty-text">
        This environment contains applications, databases or services. Viewing them is not ported yet; manage them in Coolify.
      </div>
    {:else}
      <div class="empty-title">{error.status === 404 ? 'Environment not found' : 'Could not load the environment'}</div>
      <div class="empty-text">{error.message}</div>
    {/if}
  </div></div></div>
{:else if details}
  <div class="stack">
    <div class="card">
      <div class="card-header">
        <div>
          <div class="card-title">Resources</div>
          <div class="card-description">Deploying resources from coolify-rs is not available yet.</div>
        </div>
      </div>
      <div class="card-well">
        <div class="empty">
          <div class="empty-title">No resources</div>
          <div class="empty-text">
            {resourceKinds.map(([, label]) => label).join(', ')} appear here once they exist in this environment.
          </div>
        </div>
      </div>
    </div>
    <div class="card">
      <div class="card-header"><div class="card-title">Details</div></div>
      <div class="card-well">
        <dl class="kv">
          <dt>UUID</dt><dd class="mono">{details.uuid}</dd>
          {#if details.created_at}<dt>Created</dt><dd>{new Date(details.created_at).toLocaleString()}</dd>{/if}
        </dl>
      </div>
    </div>
  </div>
{:else}
  <div class="empty"><div class="empty-text">Loading…</div></div>
{/if}
