<script lang="ts">
  // Port of App\Livewire\DeployCanvas with livewire/deploy-canvas.blade.php. The page is a shell:
  // upstream's Svelte canvas in src/upstream/js/canvas/deploy calls /canvas/api with the session.
  import { reicons } from '../upstream/reicons';
  import CanvasHost from '../components/CanvasHost.svelte';
  import ConfirmModal from '../components/ConfirmModal.svelte';
  import { get } from '../lib/serverApi';

  type Shell = {
    team_id: number;
    user_id: number;
    can_add_projects: boolean;
    service_templates: [string, string][];
    service_logos: Record<string, [string, string]>;
  };

  const ICONS = ['projects', 'servers', 'sources', 'cloud', 'file-content', 'layers', 'database', 'sliders', 'variables', 'globe', 'play-circle', 'check', 'x', 'plus', 'external-link', 'chevron-down', 'info-circle'];
  // `glob(public_path('svgs/resources/*.svg'))`.
  const RESOURCE_LOGOS = ['clickhouse', 'deploy-key', 'docker-compose', 'docker-image', 'dockerfile', 'dragonfly-dark', 'dragonfly', 'github-app-dark', 'github-app', 'gitlab-app', 'keydb-dark', 'keydb', 'mariadb', 'mongodb', 'mysql', 'postgres', 'public-repo', 'redis', 'sqlite'];

  const icons = ICONS.map((name) => `<svg viewBox="0 0 24 24" fill="none" data-icon="${name}">${(reicons as Record<string, string>)[name] ?? ''}</svg>`).join('');
  const logos = Object.fromEntries(RESOURCE_LOGOS.map((name) => [name, `/svgs/resources/${name}.svg`]));
  // JSON inside <script>: "<" must not close the element.
  const safe = (v: unknown) => JSON.stringify(v).replace(/</g, '\\u003c');
  const tag = (attr: string, v: unknown) => `<script type="application/json" ${attr}>${safe(v)}</` + `script>`;

  document.title = 'Deploy canvas | coolify-rs';

  let shell = $state<Shell | null>(null);
  let resetting = $state(false);
  get<Shell>('/canvas/deploy').then((s) => (shell = s));
</script>

{#if shell}
  <div class="h-full">
    <!-- eslint-disable-next-line svelte/no-at-html-tags -->
    {@html `<template data-deploy-canvas-icons>${icons}</template>${tag('data-deploy-canvas-services', shell.service_templates)}${tag('data-deploy-canvas-resource-logos', logos)}${tag('data-deploy-canvas-service-logos', shell.service_logos)}`}
    <CanvasHost kind="deploy" attrs={{ 'data-deploy-canvas': '', 'data-team': String(shell.team_id), 'data-can-add': shell.can_add_projects ? '1' : '0' }} class="relative h-full" />
    <div class="absolute">
      <button id="deploy-canvas-reset-trigger" type="button" class="hidden" aria-hidden="true" onclick={() => (resetting = true)}></button>
    </div>
  </div>
  {#if resetting}
    <ConfirmModal
      title="Reset the canvas?"
      warningMessage="Your steps and their settings are removed from the canvas. Resources that you already deployed stay in Coolify."
      actions={['Remove every step from the canvas.']}
      confirmWithText={false}
      step2ButtonText="Reset"
      onclose={() => (resetting = false)}
      onconfirm={async () => {
        resetting = false;
        window.dispatchEvent(new CustomEvent('deploy-canvas-reset'));
      }}
    />
  {/if}
{/if}
