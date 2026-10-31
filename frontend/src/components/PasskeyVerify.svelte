<script lang="ts">
  // Port of upstream components/auth/passkey-verify.blade.php.
  import { onDestroy, onMount } from 'svelte';
  import AuthAlert from './AuthAlert.svelte';
  import { abortPasskey, autofillPasskey, errorList, passkeysSupported, verifyPasskey, type PasskeyRoutes } from '../lib/authApi';

  let {
    routes,
    label = 'Sign in with a passkey',
    autofill = false,
    done,
  }: { routes: PasskeyRoutes; label?: string; autofill?: boolean; done: (response: unknown) => void } = $props();

  let supported = $state(false);
  let loading = $state(false);
  let error = $state<string | null>(null);

  onMount(async () => {
    supported = passkeysSupported();
    if (supported && autofill) {
      try {
        const response = await autofillPasskey(routes);
        if (response) done(response);
      } catch (e) {
        error = errorList(e)[0];
      }
    }
  });
  onDestroy(abortPasskey);

  async function verify() {
    loading = true;
    error = null;
    try {
      const response = await verifyPasskey(routes);
      if (response) done(response);
    } catch (e) {
      if (!(e instanceof Error && e.name === 'UserCancelledError')) error = errorList(e)[0];
    } finally {
      loading = false;
    }
  }
</script>

{#if supported}
  <div class="flex flex-col gap-2">
    <button class="button w-full justify-center" type="button" onclick={verify} disabled={loading}>{label}</button>
    {#if error}
      <AuthAlert type="error"><span>{error}</span></AuthAlert>
    {/if}
  </div>
{/if}
