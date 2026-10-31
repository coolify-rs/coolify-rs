<script lang="ts">
  // Port of upstream livewire/project/shared/resource-limits.blade.php.
  import FormInput from '../FormInput.svelte';
  import Reicon from '../Reicon.svelte';
  import SettingsSection from '../SettingsSection.svelte';
  import UnsavedBar from '../UnsavedBar.svelte';
  import { ApiError, request } from '../../lib/api';
  import { resPath, type ResourceKind } from '../../lib/sharedApi';
  import { notify } from '../../lib/toast.svelte';

  let { kind, uuid }: { kind: ResourceKind; uuid: string } = $props();

  const fields = ['limitsCpus', 'limitsCpuset', 'limitsCpuShares', 'limitsMemory', 'limitsMemorySwap', 'limitsMemorySwappiness', 'limitsMemoryReservation'] as const;
  let form = $state<Record<string, string> | null>(null);
  let saved = $state('');
  let canUpdate = $state(false);
  let saving = $state(false);

  const base = $derived(`${resPath(kind, uuid)}/limits`);
  const dirty = $derived(form !== null && JSON.stringify(form) !== saved);

  async function load() {
    const d = await request<any>('GET', base);
    canUpdate = d.can_update;
    form = Object.fromEntries(fields.map((f) => [f, d[f] === null || d[f] === undefined ? '' : String(d[f])]));
    saved = JSON.stringify(form);
  }

  $effect(() => {
    load();
  });

  async function submit() {
    if (saving) return;
    saving = true;
    try {
      const r = await request<{ message: string }>('POST', base, form);
      notify.success(r.message);
      await load();
    } catch (err) {
      const f = err instanceof ApiError ? err : new ApiError(0, String(err));
      if (f.errors.toasts) f.errors.toasts.forEach((m) => notify.failure(m));
      else notify.failure(f.message);
    } finally {
      saving = false;
    }
  }
</script>

{#if form}
  <form
    onsubmit={(e) => {
      e.preventDefault();
      submit();
    }}
    class="application-settings-form flex flex-col gap-6"
  >
    <UnsavedBar {dirty} {saving} onsave={submit} onreset={() => (form = JSON.parse(saved))} />
    <SettingsSection id="cpu-limits-section" title="CPU" description="Limit CPU capacity, affinity, and scheduling priority for this container.">
      {#snippet actions()}
        <a class="button" target="_blank" rel="noopener noreferrer" href="https://docs.docker.com/engine/containers/resource_constraints/#cpu">
          Docker CPU constraints
          <Reicon name="external-link" class="size-3.5" />
        </a>
      {/snippet}
      <div class="grid gap-4 md:grid-cols-3">
        <FormInput disabled={!canUpdate} placeholder="1.5" helper="Set to 0 to use all available CPUs. Decimal values such as 0.5 are supported." label="CPU limit" id="limitsCpus" bind:value={form.limitsCpus} />
        <FormInput disabled={!canUpdate} placeholder="0-2" helper="Restrict execution to specific cores, for example 0-2 or 0,1,3. Leave empty for all cores." label="CPU set" id="limitsCpuset" bind:value={form.limitsCpuset} />
        <FormInput disabled={!canUpdate} placeholder="1024" helper="Relative CPU scheduling weight. Docker uses 1024 by default." label="CPU weight" id="limitsCpuShares" bind:value={form.limitsCpuShares} />
      </div>
    </SettingsSection>
    <SettingsSection id="memory-limits-section" title="Memory" description="Set hard and soft memory limits, swap allowance, and swappiness.">
      <div class="grid gap-4 sm:grid-cols-2">
        <FormInput disabled={!canUpdate} helper="Soft reservation used when the host is under memory pressure. Use values such as 256m or 1g." label="Memory reservation" id="limitsMemoryReservation" bind:value={form.limitsMemoryReservation} />
        <FormInput disabled={!canUpdate} helper="Maximum memory available to the container. Set to 0 for unlimited." label="Memory limit" id="limitsMemory" bind:value={form.limitsMemory} />
        <FormInput disabled={!canUpdate} helper="Combined memory and swap allowance. Use values such as 512m or 2g." label="Memory and swap limit" id="limitsMemorySwap" bind:value={form.limitsMemorySwap} />
        <FormInput disabled={!canUpdate} helper="Controls how aggressively anonymous memory is swapped. Enter a value from 0 to 100." type="number" min="0" max="100" label="Swappiness" id="limitsMemorySwappiness" bind:value={form.limitsMemorySwappiness} />
      </div>
      <p class="mt-4 text-xs leading-5 text-neutral-500 dark:text-fg-dim">
        Accepted units are <code class="font-mono">b</code>, <code class="font-mono">k</code>,
        <code class="font-mono">m</code>, and <code class="font-mono">g</code>.
      </p>
    </SettingsSection>
  </form>
{/if}
