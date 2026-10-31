<script lang="ts">
  // `/profile/passkeys/confirm` of upstream routes/web.php: asks for the password when needed,
  // then shows the passkey form on /profile.
  import { untrack } from 'svelte';
  import { needsPasswordConfirmation, profileUi } from '../lib/profileApi';
  import { router } from '../lib/router.svelte';

  untrack(async () => {
    try {
      await profileUi.passkeyConfirm();
      router.go('/profile', true);
    } catch (e) {
      if (needsPasswordConfirmation(e)) router.go('/user/confirm-password?intended=/profile/passkeys/confirm', true);
      else router.go('/profile', true);
    }
  });
</script>

<main class="h-full bg-neutral-50 dark:bg-base"></main>
