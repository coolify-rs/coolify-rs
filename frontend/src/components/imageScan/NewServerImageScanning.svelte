<script lang="ts">
  // Port of upstream livewire/server/new/partials/image-scanning.blade.php (SetsImageScanning):
  // the "Scan images for vulnerabilities" choice of the create-server flows. It starts from the
  // team default and is saved on the new server.
  import Listbox from '../Listbox.svelte';
  import SettingsSection from '../SettingsSection.svelte';

  let { value = $bindable('off') }: { value?: string } = $props();
</script>

<SettingsSection
  title="Image scanning"
  description="Scan the images on this server for known vulnerabilities. You can change this later in the security settings of the server."
>
  <Listbox
    id="scan_images"
    label="Scan images for vulnerabilities"
    helper="By default, scans use up to 1 CPU and 1 GB of memory while they run, and about 1.2 GB of disk, plus temporary space for one image."
    bind:value
    options={[
      { value: 'off', label: 'Off' },
      { value: 'on', label: 'On' },
    ]}
  />
</SettingsSection>
