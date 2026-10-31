<script lang="ts">
  // Port of upstream livewire/project/shared/secret-manager-links.blade.php: the section and its
  // empty state. Linking a source reaches a third-party secret manager and is not ported.
  import Empty from '../Empty.svelte';
  import SettingsSection from '../SettingsSection.svelte';
  import { request } from '../../lib/api';
  import { resPath, type ResourceKind } from '../../lib/sharedApi';

  let { kind, uuid }: { kind: ResourceKind; uuid: string } = $props();

  let data = $state<{ linked: boolean; available_tokens: number } | null>(null);
  $effect(() => {
    request<{ linked: boolean; available_tokens: number }>('GET', `${resPath(kind, uuid)}/secret-manager`).then((d) => (data = d));
  });
</script>

{#if data}
  <div class="mt-8">
    <SettingsSection
      title="Secret manager"
      description={'Reference remote secrets in your environment variables with {{vault.KEY}}. Values are fetched at deployment time and are never stored in the Coolify database. Changing the source does not re-check existing references — missing keys fail the next deployment.'}
    >
      {#if !data.linked && data.available_tokens === 0}
        <Empty title="No secret manager tokens" description="Add a Doppler, Infisical, or HashiCorp Vault token first." iconName="keys" size="sm">
          {#snippet contents()}
            <a class="button" href="/security/integration-tokens">Integration tokens</a>
          {/snippet}
        </Empty>
      {:else}
        <Empty title="Secret manager sources are not available in coolify-rs yet" size="sm" iconName="keys" />
      {/if}
    </SettingsSection>
  </div>
{/if}
