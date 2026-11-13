<script lang="ts">
  // v4.7.5 `routes/web.php`: `/security/private-key/{private_key_uuid}` redirects to the keys
  // list, where the key opens in the editor dialog.
  import { router } from '../lib/router.svelte';

  let { uuid }: { uuid: string } = $props();

  $effect(() => {
    void uuid;
    router.go('/security/private-key', true);
  });
</script>
