<script lang="ts">
  // Port of upstream livewire/security/cloud-init-scripts.blade.php,
  // livewire/security/cloud-init-script-form.blade.php and the modal of
  // livewire/security/cloud-init-script/show.blade.php.
  import Reicon from '../components/Reicon.svelte';
  import Empty from '../components/Empty.svelte';
  import Modal from '../components/Modal.svelte';
  import FormInput from '../components/FormInput.svelte';
  import FormTextarea from '../components/FormTextarea.svelte';
  import ConfirmModal from '../components/ConfirmModal.svelte';
  import SecurityLayout from '../components/SecurityLayout.svelte';
  import SettingsSection from '../components/SettingsSection.svelte';
  import { ApiError } from '../lib/api';
  import { securityUi, ago, type ScriptRow } from '../lib/securityApi';
  import { notify } from '../lib/toast.svelte';

  document.title = 'Cloud-Init Scripts | coolify-rs';

  let scripts = $state<ScriptRow[] | null>(null);
  async function load() {
    scripts = (await securityUi.scripts()).scripts;
  }
  load();

  let adding = $state(false);
  let name = $state('');
  let script = $state('');
  let error = $state<ApiError | null>(null);

  async function create(e: SubmitEvent) {
    e.preventDefault();
    error = null;
    try {
      const r = await securityUi.createScript({ name, script });
      // The form dispatches `closeModal`, which nothing listens to: the modal stays open, empty.
      name = '';
      script = '';
      notify.success(r.message);
      await load();
    } catch (err) {
      const failure = err instanceof ApiError ? err : new ApiError(0, String(err));
      if (Object.keys(failure.errors).length) error = failure;
      else notify.failure(failure.message);
    }
  }

  let editing = $state<ScriptRow | null>(null);
  let editName = $state('');
  let editScript = $state('');
  let editError = $state<ApiError | null>(null);
  let deleting = $state(false);

  function openEditor(s: ScriptRow) {
    editing = s;
    editName = s.name;
    editScript = s.script;
    editError = null;
  }

  async function save(e: SubmitEvent) {
    e.preventDefault();
    if (!editing) return;
    editError = null;
    try {
      const r = await securityUi.updateScript(editing.uuid, { name: editName, script: editScript });
      notify.success(r.message);
      await load();
    } catch (err) {
      const failure = err instanceof ApiError ? err : new ApiError(0, String(err));
      if (Object.keys(failure.errors).length) editError = failure;
      else notify.failure(failure.message);
    }
  }

  async function remove() {
    if (!editing) return;
    try {
      await securityUi.deleteScript(editing.uuid);
      deleting = false;
      editing = null;
      await load();
    } catch (err) {
      notify.failure(err instanceof ApiError ? err.message : String(err));
    }
  }
</script>

<div>
  <SecurityLayout>
    <SettingsSection title="Cloud-init scripts" description="Reusable initialization scripts for cloud servers." flush>
      {#snippet actions()}
        <button type="button" class="button button-highlighted" onclick={() => (adding = true)}>
          <Reicon name="plus" class="size-3.5" />
          New script
        </button>
      {/snippet}
      {#if scripts}
        {#if scripts.length === 0}
          <Empty title="No cloud-init scripts" description="Create a script to reuse it during server provisioning." iconName="file-content" size="sm" />
        {:else}
          <div>
            <div class="grid grid-cols-[minmax(0,1fr)_12rem_1.75rem] items-center gap-3 border-b border-neutral-200 bg-neutral-50 px-4 py-2.5 text-[13px] font-medium text-neutral-500 dark:border-white/[0.08] dark:bg-white/[0.05] dark:text-fg-faint">
              <div class="pl-11">Script</div>
              <div>Last updated</div>
              <div class="w-7"></div>
            </div>
            {#each scripts as s (s.uuid)}
              <div class="border-b border-neutral-200 last:border-b-0 dark:border-white/[0.07]">
                <div class="grid min-h-14 w-full grid-cols-[minmax(0,1fr)_12rem_1.75rem] items-center gap-3 px-4 py-2.5 text-left transition-colors hover:bg-neutral-50 dark:hover:bg-white/[0.025]">
                  <div class="flex min-w-0 items-center gap-3">
                    <div class="flex size-8 shrink-0 items-center justify-center rounded-lg border border-neutral-200 bg-neutral-50 text-neutral-500 dark:border-white/[0.08] dark:bg-white/[0.04] dark:text-fg-dim">
                      <Reicon name="file-content" class="size-4" />
                    </div>
                    <div class="min-w-0 flex-1">
                      <h3 class="truncate text-[13px]! leading-4! font-semibold! text-black dark:text-fg">{s.name}</h3>
                    </div>
                  </div>
                  <div class="flex items-center">
                    <span class="text-[10px] text-neutral-400 dark:text-fg-faint">Updated {ago(s.updated_at)}</span>
                  </div>
                  <button type="button" class="icon-button" title="Edit cloud-init script" aria-label="Edit {s.name}" onclick={() => openEditor(s)}>
                    <Reicon name="settings" class="size-3.5" />
                  </button>
                </div>
              </div>
            {/each}
          </div>
        {/if}
      {/if}
    </SettingsSection>
  </SecurityLayout>
</div>

{#if adding}
  <Modal title="New Cloud-Init Script" onclose={() => (adding = false)}>
    <form onsubmit={create} class="application-settings-form flex w-full flex-col gap-4">
      <FormInput id="name" label="Script name" helper="A recognizable name for this reusable script." required bind:value={name} error={error?.field('name')} />
      <FormTextarea id="script" label="Script content" rows={12} monospace helper="Cloud-config YAML or another script accepted by your provider." required bind:value={script} error={error?.field('script')} />
      <div class="flex justify-end border-t border-neutral-200 pt-4 dark:border-white/[0.08]">
        <button type="submit" class="button button-highlighted">Create script</button>
      </div>
    </form>
  </Modal>
{/if}

{#if editing}
  <Modal title="Edit Cloud-Init Script" onclose={() => (editing = null)}>
    <form onsubmit={save} class="flex flex-col gap-4">
      <FormInput id="name" label="Script name" required bind:value={editName} error={editError?.field('name')} />
      <FormTextarea id="script" label="Script content" rows={16} monospace helper="Cloud-config YAML or another script accepted by your provider." required bind:value={editScript} error={editError?.field('script')} />
      <div class="flex items-center justify-between gap-2 border-t border-neutral-200 pt-4 dark:border-white/[0.08]">
        <button type="button" class="button button-error" onclick={() => (deleting = true)}>Delete</button>
        <button type="submit" class="button" {...{ isHighlighted: '' }}>Save changes</button>
      </div>
    </form>
  </Modal>
{/if}

{#if deleting && editing}
  <ConfirmModal
    title="Confirm Script Deletion?"
    actions={['This cloud-init script will be permanently deleted.']}
    confirmationText={editing.name}
    step2ButtonText="Delete script"
    onconfirm={remove}
    onclose={() => (deleting = false)}
  />
{/if}
