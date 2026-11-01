<script lang="ts">
  // Port of upstream livewire/project/shared/resource-details.blade.php (Project\Shared\ResourceDetails).
  import { untrack } from 'svelte';
  import CopyInput from '../CopyInput.svelte';
  import { request } from '../../lib/api';
  import { notify } from '../../lib/toast.svelte';

  let { uuid }: { uuid: string } = $props();

  interface Item {
    name: string;
    uuid: string;
  }
  interface Details extends Item {
    environment_name: string | null;
    environment_uuid: string | null;
    project_name: string | null;
    project_uuid: string | null;
    server_name: string | null;
    server_uuid: string | null;
    stack_applications: Item[];
    stack_databases: Item[];
  }

  let data = $state<Details | null>(null);
  $effect(() => {
    untrack(() =>
      request<Details>('GET', `/ui/resources/service/${encodeURIComponent(uuid)}/details`)
        .then((d) => (data = d))
        .catch((e) => notify.failure(String(e))),
    );
  });
</script>

{#snippet block(title: string, name: string | null, id: string | null)}
  {#if id}
    <div>
      <h3>{title}</h3>
      <div class="pt-2 grid grid-cols-1 gap-3 md:grid-cols-2">
        <CopyInput label="Name" text={name ?? ''} />
        <CopyInput label="UUID" text={id} />
      </div>
    </div>
  {/if}
{/snippet}

{#if data}
  <div class="w-full max-h-[70vh] overflow-y-auto pr-1 pt-1">
    <div class="flex flex-col gap-6">
      <div>
        <h3>Resource</h3>
        <div class="pt-2 grid grid-cols-1 gap-3 md:grid-cols-2">
          <CopyInput label="Name" text={data.name ?? ''} />
          <CopyInput label="UUID" text={data.uuid ?? ''} />
        </div>
      </div>
      {@render block('Environment', data.environment_name, data.environment_uuid)}
      {@render block('Project', data.project_name, data.project_uuid)}
      {@render block('Server', data.server_name, data.server_uuid)}
      {#if data.stack_applications.length > 0 || data.stack_databases.length > 0}
        <div>
          <h3>Stack Sub-Resources</h3>
          <div class="pt-2 grid grid-cols-1 gap-3 md:grid-cols-2">
            {#each data.stack_applications as item (item.uuid)}
              <CopyInput label="Application: {item.name}" text={item.uuid} />
            {/each}
            {#each data.stack_databases as item (item.uuid)}
              <CopyInput label="Database: {item.name}" text={item.uuid} />
            {/each}
          </div>
        </div>
      {/if}
    </div>
  </div>
{/if}
