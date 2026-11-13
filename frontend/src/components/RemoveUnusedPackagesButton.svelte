<script lang="ts" module>
  /** `RemoveUnusedPackages::supports()`. */
  export function removeUnusedSupported(packageManager: string | null | undefined): boolean {
    return ['apt', 'dnf', 'zypper'].includes(packageManager ?? '');
  }
</script>

<script lang="ts">
  // Port of upstream components/server/remove-unused-packages-button.blade.php (v4.7.5): confirms and
  // runs `removeUnusedPackages()` of the page (ManagesServerPackageUpdates). Shows nothing when the
  // package manager of the last patch check is not supported. With `trigger = false`, the dialog
  // opens from `open` (for a menu item), not from a visible button.
  import ConfirmModal from './ConfirmModal.svelte';

  let {
    packageManager,
    buttonTitle = 'Remove unused packages',
    trigger = true,
    open = $bindable(false),
    onconfirm,
  }: {
    packageManager: string | null | undefined;
    buttonTitle?: string;
    trigger?: boolean;
    open?: boolean;
    onconfirm: () => unknown;
  } = $props();
</script>

{#if removeUnusedSupported(packageManager)}
  {#if trigger}
    <button type="button" class="button flex gap-2" onclick={() => (open = true)}>{buttonTitle}</button>
  {/if}
  {#if open}
    <ConfirmModal
      title="Remove unused packages?"
      actions={['Removes packages that nothing needs anymore, and old kernels.', 'The running kernel and the newest kernel stay installed.']}
      warningMessage="coolify-rs cannot restore removed packages. Check the logs when it is done."
      confirmWithText={false}
      confirmWithPassword={false}
      step2ButtonText="Remove"
      onconfirm={async () => {
        await onconfirm();
      }}
      onclose={() => (open = false)}
    />
  {/if}
{/if}
