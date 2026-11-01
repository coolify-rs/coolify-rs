<script lang="ts">
  // Port of upstream livewire/project/shared/scheduled-task/all.blade.php and add.blade.php
  // (Project\Shared\ScheduledTask\All, Add).
  import { untrack } from 'svelte';
  import Empty from '../Empty.svelte';
  import FormInput from '../FormInput.svelte';
  import Listbox from '../Listbox.svelte';
  import Modal from '../Modal.svelte';
  import Reicon from '../Reicon.svelte';
  import SettingsSection from '../SettingsSection.svelte';
  import StatusBadge from '../StatusBadge.svelte';
  import ScheduledTaskShow from './ScheduledTaskShow.svelte';
  import { ApiError } from '../../lib/api';
  import { scheduledTasksUi, type ScheduledTasksIndex, type TaskOwnerKind } from '../../lib/scheduledTasksApi';
  import { notify } from '../../lib/toast.svelte';

  let { kind, uuid }: { kind: TaskOwnerKind; uuid: string } = $props();

  const params = new URLSearchParams(location.search);
  let data = $state<ScheduledTasksIndex | null>(null);
  // `#[Url(as: 'task_uuid')]`
  let taskUuid = $state(params.get('task_uuid') ?? '');
  let adding = $state(false);
  let missing = $state(false);
  // `Add` stays mounted while its modal is closed, so its fields keep their values.
  let form = $state({ name: '', command: '', frequency: '', container: '', timeout: '300' });
  let mounted = false;

  async function load() {
    data = await scheduledTasksUi.index(kind, uuid);
    if (!mounted) {
      mounted = true;
      if (data.container_names.length > 0) form.container = data.container_names[0];
      // `openTask()`: `firstOrFail()`.
      if (taskUuid && !data.tasks.some((t) => t.uuid === taskUuid)) missing = true;
    }
  }

  $effect(() => {
    untrack(() => load());
  });

  const selected = $derived(data?.tasks.find((t) => t.uuid === taskUuid) ?? null);

  function setUrl(next: string) {
    const q = new URLSearchParams(location.search);
    if (next) q.set('task_uuid', next);
    else q.delete('task_uuid');
    const s = q.toString();
    history.replaceState(history.state, '', `${location.pathname}${s ? `?${s}` : ''}`);
  }

  function open(next: string) {
    taskUuid = next;
    setUrl(next);
  }

  function close() {
    taskUuid = '';
    setUrl('');
    load().catch(() => {});
  }

  const statusType = (s: string | null) => (s === 'success' ? 'success' : s === 'failed' ? 'error' : s === 'running' ? 'warning' : 'neutral');
  const statusLabel = (s: string | null) => (s === 'success' ? 'Success' : s === 'failed' ? 'Failed' : s === 'running' ? 'Running' : 'Not run yet');

  // `@click="modalOpen=false"` on the submit button: the dialog closes whatever the answer is.
  async function submitAdd() {
    adding = false;
    try {
      const r = await scheduledTasksUi.add(kind, uuid, form);
      notify.success(r.message);
      // `clear()`
      form = { name: '', command: '', frequency: '', container: '', timeout: '300' };
      await load();
    } catch (e) {
      notify.failure(e instanceof ApiError ? e.message : String(e));
    }
  }
</script>

{#if missing}
  <Empty title="Scheduled task not found" description="It may have been deleted." iconName="browser-terminal" size="lg" />
{:else if data}
  <div>
    {#if selected}
      <Modal title="Edit scheduled task" subtitle={selected.name} isLarge fixedHeight focusFirstField={false} onclose={close}>
        {#key selected.uuid}
          <ScheduledTaskShow
            {kind}
            {uuid}
            task={selected}
            canUpdate={data.can_update}
            isRunning={data.is_running}
            isService={data.type === 'service'}
            timezone={data.server_timezone}
            onchanged={() => load().catch(() => {})}
            ondeleted={close}
          />
        {/key}
      </Modal>
    {/if}

    <SettingsSection id="scheduled-tasks-section" title="Scheduled tasks" description="Run commands inside this resource automatically using a cron schedule." flush>
      {#snippet actions()}
        {#if data?.can_update}
          <button type="button" class="button button-highlighted" onclick={() => (adding = true)}>
            <Reicon name="plus" class="size-3.5" />
            Add task
          </button>
        {/if}
      {/snippet}

      {#each data.tasks as task (task.uuid)}
        <div
          role="button"
          tabindex="0"
          onclick={() => open(task.uuid)}
          onkeydown={(e) => e.key === 'Enter' && open(task.uuid)}
          class="group flex cursor-pointer items-center gap-3 border-b border-neutral-200 px-4 py-3.5 transition-colors last:border-b-0 hover:bg-neutral-50 dark:border-white/[0.07] dark:hover:bg-white/[0.025]"
        >
          <div class="flex size-9 shrink-0 items-center justify-center rounded-lg bg-neutral-100 text-neutral-500 ring-1 ring-neutral-200 dark:bg-white/[0.05] dark:text-fg-dim dark:ring-white/[0.07]">
            <Reicon name="browser-terminal" class="size-[18px]" />
          </div>
          <div class="min-w-0 flex-1">
            <div class="flex flex-wrap items-center gap-2">
              <h4 class="truncate text-sm font-semibold text-black dark:text-fg">{task.name}</h4>
              {#if task.container}
                <code class="rounded bg-neutral-100 px-1.5 py-0.5 font-mono text-[11px] text-neutral-600 dark:bg-white/[0.05] dark:text-fg-dim">{task.container}</code>
              {/if}
            </div>
            <p class="mt-1 text-[13px] text-neutral-500 dark:text-fg-dim">
              Runs on
              <code class="ml-1 rounded bg-neutral-100 px-1.5 py-0.5 font-mono text-xs text-neutral-700 dark:bg-white/[0.05] dark:text-fg-dim">{task.frequency}</code>
            </p>
          </div>
          <StatusBadge label={statusLabel(task.latest_status)} type={statusType(task.latest_status)} />
        </div>
      {:else}
        <Empty title="No scheduled tasks" description="Create a task to run maintenance commands, scripts, or recurring jobs automatically." iconName="browser-terminal" />
      {/each}
    </SettingsSection>
  </div>

  {#if adding}
    {@const names = data.container_names}
    <Modal title="New scheduled task" onclose={() => (adding = false)}>
      <form
        class="flex w-full flex-col gap-4"
        onsubmit={(e) => {
          e.preventDefault();
          submitAdd();
        }}
      >
        <div class="grid gap-4 sm:grid-cols-2">
          <FormInput placeholder="Database cleanup" id="name" label="Name" bind:value={form.name} />
          <FormInput
            placeholder="0 0 * * * or daily"
            helper="Use every_minute, hourly, daily, weekly, monthly, yearly, or a cron expression. Runs in the server timezone ({data.server_timezone}). Change it in the server settings."
            id="frequency"
            label="Schedule"
            bind:value={form.frequency}
          />
        </div>

        <FormInput placeholder="php artisan schedule:run" id="command" label="Command" bind:value={form.command} />

        <div class="grid gap-4 sm:grid-cols-2">
          <FormInput type="number" placeholder="300" id="timeout" helper="Maximum execution time from 60 to 36,000 seconds." label="Timeout (seconds)" bind:value={form.timeout} />
          {#if (data.type === 'application' && names.length > 1) || data.type === 'service'}
            <Listbox id="container" label="Container" bind:value={form.container} options={names.map((n) => ({ value: n, label: n }))} />
          {:else}
            <FormInput placeholder="php" id="container" helper="Leave empty when the resource only has one container." label="Container" bind:value={form.container} />
          {/if}
        </div>

        <div class="flex justify-end border-t border-neutral-200 pt-4 dark:border-white/[0.07]">
          <button type="submit" class="button">Add task</button>
        </div>
      </form>
    </Modal>
  {/if}
{/if}
