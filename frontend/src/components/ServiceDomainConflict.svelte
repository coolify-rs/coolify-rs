<script lang="ts">
  // Port of upstream components/domain-conflict-modal.blade.php.
  import Callout from './Callout.svelte';
  import Modal from './Modal.svelte';

  let {
    conflicts,
    consequences = [
      'Only one resource will be accessible at this domain',
      'The routing behavior will be unpredictable',
      'You may experience service disruptions',
      'SSL certificates might not work correctly',
    ],
    onproceed,
    onclose,
  }: {
    conflicts: { domain: string; resource_name: string; resource_link?: string | null; resource_type: string; service_name?: string | null }[];
    consequences?: string[];
    onproceed: () => void;
    onclose: () => void;
  } = $props();
</script>

<Modal title="Domain already in use" {onclose}>
  <Callout type="danger" title="Domain conflict detected" class="mb-4">
    The following domain(s) are already in use by other resources. Using the same domain for multiple resources can cause routing conflicts and unpredictable
    behavior.
  </Callout>
  <div class="mb-4">
    <ul class="space-y-2">
      {#each conflicts as c (c.domain + c.resource_name)}
        <li class="flex items-start text-[12px] leading-5 text-red-600 dark:text-red-400">
          <div>
            <strong>{c.domain}</strong> is used by
            {#if c.resource_type === 'instance'}
              <strong>{c.resource_name}</strong>
            {:else}
              <a href={c.resource_link} target="_blank" class="underline hover:text-red-400">{c.resource_name}</a>
            {/if}
            ({c.resource_type}{#if c.service_name}: {c.service_name}{/if})
          </div>
        </li>
      {/each}
    </ul>
  </div>
  <Callout type="warning" title="What will happen if you continue?" class="mb-4">
    <ul class="mt-2 ml-4 list-disc">
      {#each consequences as line (line)}
        <li>{line}</li>
      {/each}
    </ul>
  </Callout>
  <div class="mt-4 flex flex-wrap justify-end gap-2 border-t border-neutral-200 pt-4 dark:border-white/[0.08]">
    <button type="button" class="button" onclick={onclose}>Cancel</button>
    <button type="button" class="button button-error" onclick={onproceed}>Proceed anyway</button>
  </div>
</Modal>
