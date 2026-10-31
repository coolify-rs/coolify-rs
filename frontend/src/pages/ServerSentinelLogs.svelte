<script lang="ts">
  // Port of upstream livewire/server/sentinel/logs.blade.php.
  import ServerLogsPage from './ServerLogsPage.svelte';

  let { uuid }: { uuid: string } = $props();
</script>

<ServerLogsPage {uuid} kind="sentinel" />
