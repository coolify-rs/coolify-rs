<script lang="ts">
  // Port of upstream livewire/project/shared/scheduled-task/show.blade.php (Project\Shared\ScheduledTask\Show).
  import Callout from '../Callout.svelte';
  import Checkbox from '../Checkbox.svelte';
  import ConfirmModal from '../ConfirmModal.svelte';
  import DangerZone from '../DangerZone.svelte';
  import FormInput from '../FormInput.svelte';
  import SettingsSection from '../SettingsSection.svelte';
  import UnsavedBar from '../UnsavedBar.svelte';
  import ScheduledTaskExecutions from './ScheduledTaskExecutions.svelte';
  import { ApiError } from '../../lib/api';
  import { scheduledTasksUi, type ScheduledTaskRow, type TaskOwnerKind } from '../../lib/scheduledTasksApi';
  import { notify } from '../../lib/toast.svelte';

  let {
    kind,
    uuid,
    task,
    canUpdate,
    isRunning,
    isService,
    timezone,
    onchanged,
    ondeleted,
  }: {
    kind: TaskOwnerKind;
    uuid: string;
    task: ScheduledTaskRow;
    canUpdate: boolean;
    isRunning: boolean;
    isService: boolean;
    timezone: string;
    onchanged: () => void;
    ondeleted: () => void;
  } = $props();

  const fromTask = () => ({
    name: task.name,
    frequency: task.frequency,
    timeout: String(task.timeout),
    container: task.container ?? '',
    command: task.command,
    use_shell: task.use_shell ?? true,
  });

  const sections = $derived([
    ['general', 'General'],
    ['executions', 'Executions'],
    ...(canUpdate ? [['danger', 'Danger Zone']] : []),
  ]);
  let active = $state('general');
  let form = $state(fromTask());
  let baseline = $state(JSON.stringify(fromTask()));
  let enabled = $state(task.enabled);
  let saving = $state(false);
  let deleting = $state(false);
  const dirty = $derived(JSON.stringify(form) !== baseline);

  const message = (e: unknown) => (e instanceof ApiError ? e.message : String(e));

  async function run(call: () => Promise<{ message: string }>, done?: () => void) {
    try {
      notify.success((await call()).message);
      done?.();
      onchanged();
    } catch (e) {
      notify.failure(message(e));
    }
  }

  async function submit() {
    saving = true;
    try {
      const r = await scheduledTasksUi.save(kind, uuid, task.uuid, form);
      notify.success(r.message);
      form = {
        ...form,
        name: form.name.trim(),
        command: form.command.trim(),
        frequency: form.frequency.trim(),
        container: form.container.trim(),
      };
      baseline = JSON.stringify(form);
      onchanged();
    } catch (e) {
      // `syncData(true)` puts the stored schedule back when the expression is invalid.
      if (e instanceof ApiError && e.message === 'Invalid Cron / Human expression.') {
        form.frequency = task.frequency;
      }
      notify.failure(message(e));
    } finally {
      saving = false;
    }
  }
</script>

<div class="flex min-w-0 flex-col gap-6">
  <nav aria-label="Scheduled task sections" class="flex min-w-0 flex-wrap gap-1 border-b border-neutral-200 pb-2 dark:border-white/[0.08]">
    {#each sections as [key, label] (key)}
      <button
        type="button"
        onclick={() => (active = key)}
        class="inline-flex h-8 shrink-0 cursor-pointer items-center rounded-md px-3 text-[13px] font-medium transition-colors {active === key
          ? 'bg-coollabs/10 text-coollabs ring-1 ring-coollabs/25 dark:bg-brand/15 dark:text-brand-light dark:ring-brand-light/25'
          : 'text-neutral-500 hover:bg-black/5 hover:text-neutral-900 dark:text-fg-dim dark:hover:bg-white/[0.06] dark:hover:text-fg'}"
      >
        {label}
      </button>
    {/each}
  </nav>

  {#if !enabled && task.disabled_reason}
    <Callout type="danger" title="Disabled because it keeps failing">
      {task.disabled_reason} Fix the task, then click Enable.
    </Callout>
  {/if}

  <form
    class="application-settings-form flex flex-col"
    class:hidden={active !== 'general'}
    onsubmit={(e) => {
      e.preventDefault();
      submit();
    }}
  >
    <UnsavedBar
      {dirty}
      {saving}
      onsave={submit}
      onreset={() => (form = JSON.parse(baseline))}
    />
    <SettingsSection id="task-configuration-section" title="Task configuration" description="Configure when this task runs and which command coolify-rs executes.">
      {#snippet actions()}
        {#if isRunning && canUpdate}
          <button type="button" class="button" onclick={() => run(() => scheduledTasksUi.execute(kind, uuid, task.uuid))}>Execute now</button>
        {/if}
        {#if canUpdate}
          <button
            type="button"
            class="button {enabled ? '' : 'button-highlighted'}"
            onclick={() => run(() => scheduledTasksUi.toggle(kind, uuid, task.uuid), () => (enabled = !enabled))}
          >
            {enabled ? 'Disable' : 'Enable'}
          </button>
        {/if}
      {/snippet}

      <div class="grid gap-4 md:grid-cols-2">
        <FormInput disabled={!canUpdate} placeholder="Name" id="name" label="Name" required bind:value={form.name} />
        <FormInput
          disabled={!canUpdate}
          placeholder="0 0 * * * or daily"
          id="frequency"
          label="Schedule"
          helper="Use every_minute, hourly, daily, weekly, monthly, yearly, or a cron expression. Runs in the server timezone ({timezone}). Change it in the server settings."
          required
          bind:value={form.frequency}
        />
        <FormInput disabled={!canUpdate} type="number" placeholder="300" id="timeout" helper="Maximum execution time from 60 to 36,000 seconds." label="Timeout (seconds)" required bind:value={form.timeout} />
        <FormInput disabled={!canUpdate} placeholder="php" helper="Leave empty when the resource only has one container." id="container" label={isService ? 'Service' : 'Container'} bind:value={form.container} />
      </div>
      <div class="mt-4">
        <FormInput disabled={!canUpdate} placeholder="php artisan schedule:run" id="command" label="Command" required bind:value={form.command} />
      </div>
      <div class="mt-2 md:w-1/2">
        <Checkbox
          disabled={!canUpdate}
          id="useShell"
          label="Run in a shell"
          helper="Run the command with <span class='font-mono'>sh -c</span>, so you can use variables, pipes, and redirects. Turn it off for containers without a shell (distroless or scratch images): Coolify then splits the command into arguments and runs it directly."
          bind:checked={form.use_shell}
        />
      </div>
    </SettingsSection>
  </form>

  <div class:hidden={active !== 'executions'}>
    <SettingsSection id="task-executions-section" title="Recent executions" description="Execution history refreshes automatically while a task is running." flush>
      <ScheduledTaskExecutions {kind} {uuid} task={task.uuid} {timezone} />
    </SettingsSection>
  </div>

  {#if canUpdate}
    <div class="application-settings-form" class:hidden={active !== 'danger'}>
      <SettingsSection title="Delete scheduled task" description="Permanently remove this task and its execution history.">
        <DangerZone title="This action cannot be undone.">
          <p>The task stops running and cannot be restored.</p>
          {#snippet action()}
            <button type="button" class="button button-error" onclick={() => (deleting = true)}>Delete task</button>
          {/snippet}
        </DangerZone>
      </SettingsSection>
    </div>
  {/if}
</div>

{#if deleting}
  <ConfirmModal
    title="Delete scheduled task?"
    actions={['The selected scheduled task will be permanently deleted.']}
    confirmationText={task.name}
    confirmationLabel="Enter the scheduled task name to confirm deletion"
    shortConfirmationLabel="Task name"
    confirmWithPassword={false}
    step2ButtonText="Permanently delete"
    onconfirm={async () => {
      await run(() => scheduledTasksUi.remove(kind, uuid, task.uuid), ondeleted);
    }}
    onclose={() => (deleting = false)}
  />
{/if}
