<script lang="ts">
  import type { Snippet } from 'svelte';
  import Icon from './Icon.svelte';
  import { router, link } from '../lib/router.svelte';
  import { session } from '../lib/session.svelte';
  import { sessionApi } from '../lib/api';
  import { notify } from '../lib/toast.svelte';

  let { children }: { children: Snippet } = $props();

  let teamMenu = $state(false);
  let userMenu = $state(false);
  let dark = $state(document.documentElement.classList.contains('dark'));

  const nav = [
    { href: '/projects', label: 'Projects', icon: 'folder', match: ['projects', 'project', 'environment'] },
    { href: '/security/keys', label: 'Private keys', icon: 'key', match: ['keys'] },
    { href: '/team', label: 'Team', icon: 'users', match: ['team'] },
  ];

  function toggleTheme() {
    dark = !dark;
    document.documentElement.classList.toggle('dark', dark);
    try {
      localStorage.setItem('theme', dark ? 'dark' : 'light');
    } catch {
      // Storage may be unavailable (private mode); the toggle still works.
    }
  }

  async function switchTeam(id: number) {
    teamMenu = false;
    try {
      session.set(await sessionApi.switchTeam(id));
      router.go('/projects');
    } catch (e) {
      notify.error('Could not switch team', String(e));
    }
  }

  async function logout() {
    userMenu = false;
    try {
      await sessionApi.logout();
    } finally {
      session.set(null);
      router.go('/login', true);
    }
  }

  function closeMenus(e: MouseEvent) {
    if (!(e.target as HTMLElement).closest('.menu-wrap')) {
      teamMenu = false;
      userMenu = false;
    }
  }
</script>

<svelte:window onclick={closeMenus} />

<div class="shell">
  <div class="logo-cell">
    <span class="wordmark">coolify-rs</span>
    <span class="version">{session.info?.version}</span>
  </div>
  <header class="topbar">
    <div class="menu-wrap">
      <button class="button button-ghost" onclick={() => (teamMenu = !teamMenu)} aria-haspopup="menu">
        {session.info?.current_team?.name ?? 'No team'}
        <span class="cell-faint">{session.info?.current_team?.role}</span>
      </button>
      {#if teamMenu}
        <div class="menu" role="menu">
          <div class="menu-label">Switch team</div>
          {#each session.info?.teams ?? [] as t (t.id)}
            <button class="menu-option" role="menuitem" onclick={() => switchTeam(t.id)}>
              {t.name}
              {#if t.id === session.info?.current_team?.id}<span aria-label="current">✓</span>{/if}
            </button>
          {/each}
        </div>
      {/if}
    </div>
    <button class="button button-ghost" onclick={toggleTheme} title="Toggle theme" aria-label="Toggle theme">
      <Icon name={dark ? 'sun' : 'moon'} />
    </button>
    <div class="menu-wrap">
      <button class="button button-ghost" onclick={() => (userMenu = !userMenu)} aria-haspopup="menu">
        {session.info?.user.name}
      </button>
      {#if userMenu}
        <div class="menu" role="menu">
          <div class="menu-label">{session.info?.user.email}</div>
          <button class="menu-option" role="menuitem" onclick={logout}>
            Sign out <Icon name="logout" />
          </button>
        </div>
      {/if}
    </div>
  </header>
  <nav class="sidebar" aria-label="Main">
    <div class="nav-section">Workspace</div>
    {#each nav as item (item.href)}
      <a
        href={item.href}
        use:link
        class="nav-item"
        class:is-active={item.match.includes(router.current.name)}
        aria-current={item.match.includes(router.current.name) ? 'page' : undefined}
      >
        <span class="nav-tile"><Icon name={item.icon} /></span>
        {item.label}
      </a>
    {/each}
  </nav>
  <main class="main">
    {#if session.info?.features.read_only}
      <div class="notice" style="margin-bottom:16px">
        coolify-rs is in read-only mode: the database schema does not match this build. Changes are disabled.
      </div>
    {/if}
    {#if session.info?.current_team?.two_factor_blocked}
      <div class="notice notice-error" style="margin-bottom:16px">
        This team requires two-factor authentication. Enable it for your account in Coolify, then sign in again.
      </div>
    {/if}
    {@render children()}
  </main>
</div>
