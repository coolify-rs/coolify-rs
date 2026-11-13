<script lang="ts">
  // Port of upstream livewire/star-toggle.blade.php (App\Livewire\StarToggle): stars or unstars a
  // resource for the logged-in user. Starred resources show on the dashboard.
  import Reicon from './Reicon.svelte';
  import { stars, type StarKind } from '../lib/starsApi.svelte';
  import { notify } from '../lib/toast.svelte';

  let { kind, uuid }: { kind: StarKind; uuid: string } = $props();

  const starred = $derived(stars.starred[stars.key(kind, uuid)] === true);
  // `wire:loading.attr="disabled"`
  let busy = $state(false);

  $effect(() => {
    void stars.load(kind, uuid);
  });

  async function toggle() {
    busy = true;
    try {
      await stars.toggle(kind, uuid);
    } catch (e) {
      notify.error(e instanceof Error ? e.message : 'Something went wrong.');
    } finally {
      busy = false;
    }
  }
</script>

<button
  type="button"
  onclick={toggle}
  disabled={busy}
  title={starred ? 'Remove from the dashboard' : 'Add to the dashboard'}
  aria-label={starred ? 'Unstar' : 'Star'}
  aria-pressed={starred ? 'true' : 'false'}
  class="flex size-7 shrink-0 items-center justify-center rounded-md transition-colors hover:bg-neutral-100 dark:hover:bg-white/[0.05] {starred
    ? 'text-warning'
    : 'text-neutral-400 hover:text-black dark:text-fg-faint dark:hover:text-fg'}"
>
  <Reicon name={starred ? 'star-filled' : 'star'} class="size-4" />
</button>
