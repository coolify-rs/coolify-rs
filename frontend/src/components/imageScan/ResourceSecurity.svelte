<script lang="ts">
  // The Security page of an application, service, or database (`project.{type}.vulnerabilities`
  // of upstream livewire/project/{application,service,database}/configuration.blade.php): the
  // findings, the packages, and the scan settings of the resource.
  import Packages from './Packages.svelte';
  import ScanSettings from './ScanSettings.svelte';
  import Vulnerabilities from './Vulnerabilities.svelte';
  import type { ResourceKind } from '../../lib/imageScanApi';

  let { kind, uuid, onchanged }: { kind: ResourceKind; uuid: string; onchanged?: () => void } = $props();

  /** Bumped when a scan or a setting changed what the other sections show. */
  let version = $state(0);
  let vulnerabilities = $state(0);

  function changed() {
    version++;
    onchanged?.();
  }
</script>

<div class="flex flex-col gap-6">
  {#key vulnerabilities}
    <Vulnerabilities {kind} {uuid} onchanged={changed} />
  {/key}
  <Packages {kind} {uuid} {version} />
  <ScanSettings
    scope={kind}
    {uuid}
    onsaved={() => {
      // A rule or an ignore changes the status and the findings.
      vulnerabilities++;
      changed();
    }}
  />
</div>
