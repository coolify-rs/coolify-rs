<script lang="ts">
  // Port of upstream livewire/image-scan/team-image-scanning.blade.php
  // (ImageScan\TeamImageScanning): the scan rules and ignores of the session team. Members see
  // the values, admins edit them.
  import TeamLayout from '../components/TeamLayout.svelte';
  import ScanSettings from '../components/imageScan/ScanSettings.svelte';

  document.title = 'Team Image Scanning | coolify-rs';
</script>

<div>
  <TeamLayout>
    <div class="application-settings-form flex flex-col gap-6">
      <ScanSettings scope="team" />
    </div>
  </TeamLayout>
</div>
