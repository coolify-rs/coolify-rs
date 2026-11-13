<script lang="ts">
  // Port of upstream livewire/image-scan/instance-image-scanning.blade.php
  // (ImageScan\InstanceImageScanning): the default scan rules of every team. Only for instance
  // admins; others go to the dashboard.
  import SettingsLayout from '../components/SettingsLayout.svelte';
  import ScanSettings from '../components/imageScan/ScanSettings.svelte';
  import { router } from '../lib/router.svelte';
  import { session } from '../lib/session.svelte';

  document.title = 'Image Scanning | coolify-rs';
  $effect(() => {
    if (session.info && !session.isInstanceAdmin) router.go('/', true);
  });
</script>

{#if session.isInstanceAdmin}
  <SettingsLayout>
    <div class="application-settings-form flex min-w-0 flex-col gap-6">
      <ScanSettings scope="instance" />
    </div>
  </SettingsLayout>
{/if}
