<script lang="ts">
  // Port of upstream livewire/security/private-key/show.blade.php (page mode).
  import Empty from '../components/Empty.svelte';
  import SecurityLayout from '../components/SecurityLayout.svelte';
  import KeyForm from '../components/KeyForm.svelte';
  import { api, keysUi, ApiError, type PrivateKey } from '../lib/api';
  import { router } from '../lib/router.svelte';
  import { session } from '../lib/session.svelte';

  let { uuid }: { uuid: string } = $props();

  let key = $state<(PrivateKey & { private_key?: string }) | null>(null);
  let inUse = $state(false);
  let missing = $state(false);

  $effect(() => {
    Promise.all([api.get<PrivateKey & { private_key?: string }>(`/security/keys/${encodeURIComponent(uuid)}`), keysUi.list()])
      .then(([k, list]) => {
        key = k;
        inUse = list.keys.find((x) => x.uuid === uuid)?.in_use ?? false;
        document.title = `${k.name} | Private Keys | coolify-rs`;
      })
      .catch((e) => {
        if (e instanceof ApiError && (e.status === 404 || e.status === 403)) missing = true;
        else throw e;
      });
  });
</script>

{#if missing}
  <div class="application-settings-form w-full">
    <Empty title="Private key not found" description="It may have been deleted, or you cannot view it." iconName="keys" size="lg" />
  </div>
{:else if key}
  <div>
    <SecurityLayout>
      <KeyForm {key} {inUse} canEdit={session.isAdmin} ondeleted={() => router.go('/security/private-key')} />
    </SecurityLayout>
  </div>
{/if}
