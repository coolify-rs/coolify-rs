<script lang="ts">
  // Port of the `$type === 'server'` side of upstream execute-container-command.blade.php.
  import ExecuteContainerCommand from '../components/shared/ExecuteContainerCommand.svelte';
  import ServerFrame from '../components/ServerFrame.svelte';

  let { uuid }: { uuid: string } = $props();
</script>

<ServerFrame {uuid} active="terminal" title="Terminal">
  {#snippet children()}
    <ExecuteContainerCommand kind="server" {uuid} />
  {/snippet}
</ServerFrame>
