<script lang="ts">
  // Port of upstream livewire/server/new/by-digital-ocean.blade.php (`Server\New\ByDigitalOcean`).
  import Checkbox from '../components/Checkbox.svelte';
  import CloudInitFields from '../components/CloudInitFields.svelte';
  import CloudPrivateKey from '../components/CloudPrivateKey.svelte';
  import CloudProvision from '../components/CloudProvision.svelte';
  import FormInput from '../components/FormInput.svelte';
  import Listbox from '../components/Listbox.svelte';
  import SettingsSection from '../components/SettingsSection.svelte';
  import { ApiError } from '../lib/api';
  import { cloudUi, filled, intOrNull, numberFormat, optionValue, plural, type CloudForm, type Item } from '../lib/cloudApi';
  import { failure } from '../lib/serverApi';
  import { router } from '../lib/router.svelte';

  type Data = { regions: Item[]; sizes: Item[]; images: Item[]; ssh_keys: Item[] };
  let { tokenUuid }: { tokenUuid?: string } = $props();

  let d = $state<Data>({ regions: [], sizes: [], images: [], ssh_keys: [] });
  let serverName = $state('');
  let region = $state('');
  let size = $state('');
  let image = $state('');
  let privateKey = $state('');
  let sshKeys = $state<number[]>([]);
  let enableIpv6 = $state(true);
  let monitoring = $state(true);
  let showScript = $state(false);
  let script = $state('');
  let scriptId = $state('');
  let saveScript = $state(false);
  let scriptName = $state('');
  let error = $state<ApiError | null>(null);
  let busy = $state(false);

  function init(form: CloudForm, data: Data) {
    d = data;
    serverName = form.name;
    if (form.keys.length) privateKey = String(form.keys[0].id);
  }

  /** `getAvailableSizesProperty`. */
  const availableSizes = $derived(region ? d.sizes.filter((s) => (s.regions ?? []).includes(region)) : d.sizes);
  /** `getAvailableImagesProperty`. */
  const availableImages = $derived(region ? d.images.filter((i) => (i.regions ?? []).includes(region)) : d.images);
  const imageOptions = $derived(
    availableImages.map((i) => ({
      value: i.slug ?? i.id,
      label: `${i.distribution ?? ''} ${i.name ?? i.slug ?? i.id}`.trim(),
    })),
  );

  /** `getSelectedDropletPriceProperty`. */
  const price = $derived.by(() => {
    const cost = d.sizes.find((s) => s.slug === size)?.price_monthly;
    return size && cost !== null && cost !== undefined ? `$${numberFormat(cost)}` : null;
  });

  /** `getAdvancedDigitalOceanOptionsSummaryProperty`. */
  const summary = $derived.by(() => {
    const out: string[] = [];
    if (sshKeys.length > 0) out.push(`${sshKeys.length} extra SSH ${plural('key', sshKeys.length)}`);
    if (!enableIpv6) out.push('IPv4 only');
    if (!monitoring) out.push('Monitoring off');
    if (showScript || filled(script) || filled(scriptId)) out.push('Cloud-init');
    return out;
  });

  const sizeLabel = (s: Item) => {
    const label = `${s.slug} · ${s.memory ?? '?'} MB RAM · ${s.vcpus ?? '?'} vCPU${s.disk !== null && s.disk !== undefined ? ` · ${s.disk} GB` : ''}`;
    return s.price_monthly !== null && s.price_monthly !== undefined ? `${label} · $${numberFormat(s.price_monthly)}/mo` : label;
  };

  async function submit(e: SubmitEvent) {
    e.preventDefault();
    busy = true;
    error = null;
    try {
      const { uuid } = await cloudUi.create('digitalocean', tokenUuid as string, {
        server_name: serverName,
        selected_region: region,
        selected_image: optionValue(imageOptions, image),
        selected_size: size,
        private_key_id: intOrNull(privateKey),
        selectedDigitalOceanSshKeyIds: sshKeys,
        enable_ipv6: enableIpv6,
        monitoring,
        show_cloud_init_script: showScript,
        cloud_init_script: script,
        save_cloud_init_script: saveScript,
        cloud_init_script_name: scriptName,
        selected_cloud_init_script_id: intOrNull(scriptId),
      });
      router.go(`/server/${uuid}`);
    } catch (err) {
      error = failure(err);
    } finally {
      busy = false;
    }
  }
</script>

<CloudProvision
  provider="digitalocean"
  providerLabel="DigitalOcean"
  routeType="digital-ocean"
  {tokenUuid}
  onready={init}
  loadingDescription="Fetching regions, sizes, images, and account resources."
  signupUrl="https://coolify.io/digitalocean"
  signupText="through Coolify's referral link."
>
  {#snippet children(form: CloudForm, refreshKeys)}
    <form onsubmit={submit} class="flex flex-col gap-6">
      <SettingsSection title="DigitalOcean Droplet" description="Choose the region, size, image, and Coolify SSH key.">
        {#snippet actions()}
          <button type="submit" class="button button-highlighted" disabled={!privateKey || busy}>
            Buy and create
            {#if price}<span class="opacity-70">· {price}/mo</span>{/if}
          </button>
        {/snippet}
        <div class="grid gap-4 lg:grid-cols-2">
          <div class="lg:col-span-2">
            <FormInput id="server_name" label="Server name" helper="A friendly name shown in Coolify." bind:value={serverName} error={error?.field('server_name')} />
          </div>
          <Listbox
            id="selected_region"
            label="Region"
            required
            placeholder="Select a region"
            bind:value={region}
            onchange={() => ((size = ''), (image = ''))}
            error={error?.field('selected_region')}
            options={d.regions.map((r) => ({ value: r.slug, label: r.name ?? r.slug }))}
          />
          <Listbox
            id="selected_size"
            label="Size"
            required
            disabled={!region}
            placeholder="Select a size"
            bind:value={size}
            onchange={() => (image = '')}
            error={error?.field('selected_size')}
            options={availableSizes.map((s) => ({ value: s.slug, label: sizeLabel(s) }))}
          />
          <Listbox id="selected_image" label="Image" required disabled={!size} placeholder="Select an image" bind:value={image} error={error?.field('selected_image')} options={imageOptions} />
          <CloudPrivateKey keys={form.keys} bind:value={privateKey} helper="This key is added to the Droplet automatically." error={error?.field('private_key_id')} refresh={refreshKeys} />
        </div>
      </SettingsSection>

      <SettingsSection title="Advanced options" description="Provider SSH keys, networking, monitoring, and cloud-init.">
        {#if summary.length > 0}
          <div class="mb-4 flex flex-wrap gap-1.5">
            {#each summary as item (item)}
              <span class="rounded-full bg-neutral-100 px-2 py-0.5 text-[10px] font-medium text-neutral-600 dark:bg-white/[0.06] dark:text-fg-dim">{item}</span>
            {/each}
          </div>
        {/if}
        <div class="flex flex-col gap-4">
          <Listbox
            id="selectedDigitalOceanSshKeyIds"
            label="Extra SSH keys"
            helper="Existing keys from the DigitalOcean account."
            multiple
            searchable
            disabled={d.ssh_keys.length === 0}
            searchPlaceholder="Search SSH keys…"
            placeholder={d.ssh_keys.length ? 'Select SSH keys' : 'No account keys found'}
            bind:values={sshKeys}
            error={error?.field('selectedDigitalOceanSshKeyIds')}
            options={d.ssh_keys.map((k) => ({ value: k.id, label: k.name ?? k.fingerprint }))}
          />
          <div class="grid gap-3 lg:grid-cols-2">
            <Checkbox id="enable_ipv6" label="Enable IPv6" fullWidth bind:checked={enableIpv6} />
            <Checkbox id="monitoring" label="Enable DigitalOcean monitoring" fullWidth bind:checked={monitoring} />
          </div>
          <CloudInitFields scripts={form.scripts} bind:show={showScript} bind:script bind:selectedId={scriptId} bind:save={saveScript} bind:name={scriptName} {error} />
        </div>
      </SettingsSection>
    </form>
  {/snippet}
</CloudProvision>
