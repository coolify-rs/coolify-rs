<script lang="ts">
  // Port of upstream livewire/server/registries.blade.php.
  import RegistryCard from '../components/RegistryCard.svelte';
  import ServerFrame from '../components/ServerFrame.svelte';

  let { uuid }: { uuid: string } = $props();
</script>

<ServerFrame {uuid} active="registries" title="Registries">
  {#snippet children(f)}
    <div class="application-settings-form flex w-full flex-col gap-3">
      <RegistryCard server={{ uuid: f.uuid, name: f.name, functional: f.functional }} />
      <p class="text-[12px] text-neutral-500 dark:text-fg-dim">
        Registry logins are read from the Docker config that deployments use. Use <b>Log in</b>, or run <code>docker login &lt;registry&gt;</code> on the server.
      </p>
    </div>
  {/snippet}
</ServerFrame>
