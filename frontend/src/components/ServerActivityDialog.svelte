<script lang="ts">
  // `<x-process-dialog>` around `livewire:activity-monitor fullHeight`, following
  // the activity that a server action started.
  import { onDestroy } from 'svelte';
  import DatabaseActivityMonitor from './DatabaseActivityMonitor.svelte';
  import ProcessDialog from './ProcessDialog.svelte';
  import type { DatabaseState } from '../lib/api';
  import { get } from '../lib/serverApi';

  let { uuid, activityId, title, onclose }: { uuid: string; activityId: number | null; title: string; onclose: () => void } = $props();

  let activity = $state<DatabaseState['activity']>(null);
  let timer: ReturnType<typeof setInterval> | undefined;

  async function poll() {
    if (activityId === null) return;
    const r = await get<{ activity: DatabaseState['activity'] }>(`/server-page/${encodeURIComponent(uuid)}/activity/${activityId}`).catch(() => null);
    if (r) activity = r.activity;
    if (activity && activity.status !== 'queued' && activity.status !== 'in_progress') clearInterval(timer);
  }
  $effect(() => {
    void activityId;
    clearInterval(timer);
    activity = null;
    poll();
    timer = setInterval(poll, 1000);
  });
  onDestroy(() => clearInterval(timer));
</script>

<ProcessDialog {title} closeWithX size="xl" {onclose}>
  <div class="flex h-full min-h-0 flex-col">
    <DatabaseActivityMonitor {activity} />
  </div>
</ProcessDialog>
