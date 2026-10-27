<script lang="ts">
  import Shell from './components/Shell.svelte';
  import Login from './pages/Login.svelte';
  import Projects from './pages/Projects.svelte';
  import Project from './pages/Project.svelte';
  import Environment from './pages/Environment.svelte';
  import Keys from './pages/Keys.svelte';
  import Team from './pages/Team.svelte';
  import { router, link } from './lib/router.svelte';
  import { session } from './lib/session.svelte';
  import { toasts } from './lib/toast.svelte';
  import { setUnauthenticatedHandler } from './lib/api';

  session.refresh();
  setUnauthenticatedHandler(() => {
    session.set(null);
    router.go('/login', true);
  });

  // Route guard: signed-out users see the login page; signed-in users skip it.
  $effect(() => {
    if (!session.loaded) return;
    const name = router.current.name;
    if (!session.info && name !== 'login') router.go('/login', true);
    else if (session.info && (name === 'login' || name === 'home')) router.go('/projects', true);
  });
</script>

{#if !session.loaded}
  <div class="auth"><div class="empty-text">Loading…</div></div>
{:else if !session.info || router.current.name === 'login'}
  <Login />
{:else}
  <Shell>
    {#if router.current.name === 'projects'}
      <Projects />
    {:else if router.current.name === 'project'}
      {#key router.current.params.uuid}<Project uuid={router.current.params.uuid} />{/key}
    {:else if router.current.name === 'environment'}
      {#key router.current.params.env}
        <Environment uuid={router.current.params.uuid} env={router.current.params.env} />
      {/key}
    {:else if router.current.name === 'keys'}
      <Keys />
    {:else if router.current.name === 'team'}
      <Team />
    {:else}
      <div class="card"><div class="card-well"><div class="empty">
        <div class="empty-title">Page not found</div>
        <a class="button" href="/projects" use:link>Go to projects</a>
      </div></div></div>
    {/if}
  </Shell>
{/if}

<div class="toasts" aria-live="polite">
  {#each toasts.items as t (t.id)}
    <div class="toast toast-{t.kind}">
      <span class="badge-dot" style="margin-top:6px"></span>
      <div>
        <div style="font-weight:500">{t.title}</div>
        {#if t.description}<div class="cell-faint">{t.description}</div>{/if}
      </div>
      <button class="button button-ghost" style="height:20px;padding:0 4px" onclick={() => toasts.dismiss(t.id)} aria-label="Dismiss">✕</button>
    </div>
  {/each}
</div>
