<script lang="ts">
  // Port of upstream components/forms/domain-input.blade.php (the S3 endpoint form).
  import Listbox from './Listbox.svelte';
  import type { EndpointParts } from '../lib/storagesApi';

  let { parts = $bindable() }: { parts: EndpointParts } = $props();
</script>

<div class="grid gap-4 sm:grid-cols-[8rem_minmax(0,1fr)_8rem]">
  <div class="min-w-0">
    <Listbox
      id="endpointParts-protocol"
      label="Protocol"
      bind:value={parts.scheme}
      options={[
        { value: 'https', label: 'https' },
        { value: 'http', label: 'http' },
      ]}
    />
  </div>

  <div class="min-w-0">
    <div class="mb-1.5 flex h-4 w-full items-center gap-1.5">
      <label for="endpointParts-host" class="mb-0! flex items-center gap-1.5 leading-4">
        Host <span class="text-helper">*</span>
      </label>
    </div>
    <input id="endpointParts-host" type="text" class="input" bind:value={parts.host} placeholder="minio.internal or 192.168.1.50" autocomplete="off" required />
  </div>

  <div class="min-w-0">
    <div class="mb-1.5 flex h-4 w-full items-center gap-1.5">
      <label for="endpointParts-port" class="mb-0! flex items-center gap-1.5 leading-4">Port</label>
    </div>
    <input id="endpointParts-port" type="number" class="input" bind:value={parts.port} placeholder="3000" min="1" max="65535" inputmode="numeric" />
  </div>

  <div class="min-w-0 sm:col-span-3">
    <div class="mb-1.5 flex h-4 w-full items-center gap-1.5">
      <label for="endpointParts-path" class="mb-0! flex items-center gap-1.5 leading-4">Path</label>
    </div>
    <input id="endpointParts-path" type="text" class="input" bind:value={parts.path} placeholder="/api/v3" autocomplete="off" />
    <p class="mt-1 text-[12px] text-neutral-500 dark:text-fg-dim">Optional path, query, or fragment appended after the domain and port.</p>
  </div>
</div>
