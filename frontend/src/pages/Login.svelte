<script lang="ts">
  import { ApiError, sessionApi, type SessionInfo } from '../lib/api';
  import { session } from '../lib/session.svelte';
  import { router } from '../lib/router.svelte';
  import { focusOnMount } from '../lib/focus';

  let email = $state('');
  let password = $state('');
  let code = $state('');
  let needsCode = $state(false);
  let busy = $state(false);
  let error = $state<ApiError | null>(null);

  async function submit(e: SubmitEvent) {
    e.preventDefault();
    busy = true;
    error = null;
    try {
      const res = await sessionApi.login({ email, password, code: needsCode ? code : undefined });
      if ('two_factor' in res) {
        needsCode = true;
        return;
      }
      session.set(res as SessionInfo);
      router.go('/projects', true);
    } catch (err) {
      error = err instanceof ApiError ? err : new ApiError(0, String(err));
      if (needsCode) code = '';
    } finally {
      busy = false;
    }
  }
</script>

<div class="auth">
  <div class="card">
    <div class="card-header">
      <div>
        <div class="card-title">{needsCode ? 'Two-factor authentication' : 'Sign in to coolify-rs'}</div>
        <div class="card-description">
          {needsCode ? 'Enter the code from your authenticator app.' : 'Use your Coolify account.'}
        </div>
      </div>
    </div>
    <form class="card-well" onsubmit={submit}>
      {#if !needsCode}
        <div class="field">
          <label class="label" for="email">Email</label>
          <input id="email" class="input" use:focusOnMount type="email" autocomplete="username" bind:value={email} required />
          {#if error?.field('email')}<div class="field-error">{error.field('email')}</div>{/if}
        </div>
        <div class="field">
          <label class="label" for="password">Password</label>
          <input id="password" class="input" type="password" autocomplete="current-password" bind:value={password} required />
          {#if error?.field('password')}<div class="field-error">{error.field('password')}</div>{/if}
        </div>
      {:else}
        <div class="field">
          <label class="label" for="code">Code</label>
          <input id="code" class="input mono" use:focusOnMount inputmode="numeric" autocomplete="one-time-code" maxlength="6" bind:value={code} required />
          {#if error?.field('code')}<div class="field-error">{error.field('code')}</div>{/if}
        </div>
      {/if}
      {#if error && !error.field('email') && !error.field('password') && !error.field('code')}
        <div class="notice notice-error" style="margin-top:16px">{error.message}</div>
      {/if}
      <div class="row" style="margin-top:20px">
        {#if needsCode}
          <button type="button" class="button" onclick={() => ((needsCode = false), (code = ''))}>Back</button>
        {/if}
        <span class="spacer"></span>
        <button class="button button-primary" type="submit" disabled={busy}>
          {busy ? 'Signing in…' : needsCode ? 'Verify' : 'Sign in'}
        </button>
      </div>
    </form>
  </div>
</div>
