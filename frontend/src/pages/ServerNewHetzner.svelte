<script lang="ts">
  // Port of upstream livewire/server/new/by-hetzner.blade.php (`Server\New\ByHetzner`).
  import Checkbox from '../components/Checkbox.svelte';
  import CloudInitFields from '../components/CloudInitFields.svelte';
  import CloudPrivateKey from '../components/CloudPrivateKey.svelte';
  import CloudProvision from '../components/CloudProvision.svelte';
  import FormInput from '../components/FormInput.svelte';
  import Listbox from '../components/Listbox.svelte';
  import SettingsSection from '../components/SettingsSection.svelte';
  import { ApiError } from '../lib/api';
  import { cloudUi, filled, intOrNull, numberFormat, plural, truthy, type CloudForm, type Item } from '../lib/cloudApi';
  import { failure } from '../lib/serverApi';
  import { router } from '../lib/router.svelte';

  type Data = { locations: Item[]; server_types: Item[]; images: Item[]; ssh_keys: Item[]; firewalls: Item[]; networks: Item[] };
  let { tokenUuid }: { tokenUuid?: string } = $props();

  let d = $state<Data>({ locations: [], server_types: [], images: [], ssh_keys: [], firewalls: [], networks: [] });
  let serverName = $state('');
  let location = $state('');
  let serverType = $state('');
  let image = $state('');
  let privateKey = $state('');
  let sshKeys = $state<number[]>([]);
  let firewalls = $state<number[]>([]);
  let networks = $state<number[]>([]);
  let enableIpv4 = $state(true);
  let enableIpv6 = $state(true);
  let enableBackups = $state(false);
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

  /** `getAvailableServerTypesProperty`. */
  const availableServerTypes = $derived(location ? d.server_types.filter((t) => t.locations?.some((l: Item) => l.name === location)) : d.server_types);

  /** `getAvailableImagesProperty`. */
  const availableImages = $derived.by(() => {
    if (!serverType) return d.images;
    const architecture = d.server_types.find((t) => t.name === serverType)?.architecture;
    if (architecture === null || architecture === undefined) return d.images;
    return d.images.filter((i) => (i.architecture ?? null) === architecture);
  });

  /** `getAvailableNetworksProperty`. */
  const availableNetworks = $derived.by(() => {
    const attachable = (s: Item) => ['cloud', 'server'].includes(s.type);
    const networksList = d.networks.filter((n) => (n.subnets ?? []).some(attachable));
    if (!location) return networksList;
    const zone = d.locations.find((l) => l.name === location)?.network_zone;
    if (!truthy(zone)) return networksList;
    return networksList.filter((n) => (n.subnets ?? []).some((s: Item) => attachable(s) && s.network_zone === zone));
  });

  /** `getSelectedServerPriceProperty`. */
  const price = $derived.by(() => {
    const gross = d.server_types.find((t) => t.name === serverType)?.prices?.[0]?.price_monthly?.gross;
    return serverType && gross !== null && gross !== undefined ? `€${numberFormat(gross)}` : null;
  });

  /** `getAdvancedHetznerOptionsSummaryProperty`. */
  const summary = $derived.by(() => {
    const out: string[] = [];
    if (sshKeys.length > 0) out.push(`${sshKeys.length} extra SSH ${plural('key', sshKeys.length)}`);
    if (firewalls.length > 0) out.push(`${firewalls.length} ${plural('firewall', firewalls.length)}`);
    if (networks.length > 0) out.push(`${networks.length} private ${plural('network', networks.length)}`);
    if (enableBackups) out.push('Backups on');
    if (!enableIpv4 || !enableIpv6) out.push([enableIpv4 ? 'IPv4' : null, enableIpv6 ? 'IPv6' : null].filter(Boolean).join(' + ') || 'No public IP');
    if (showScript || filled(script) || filled(scriptId)) out.push('Cloud-init');
    return out;
  });

  /** `updatedSelectedLocation`. */
  function locationChanged() {
    serverType = '';
    image = '';
    networks = networks.filter((id) => availableNetworks.some((n) => n.id === id));
  }

  const typeLabel = (t: Item) => {
    const label = `${t.description ?? t.name} · ${t.cores ?? '?'} vCPU · ${t.memory ?? '?'} GB RAM · ${t.disk ?? '?'} GB`;
    const gross = t.prices?.[0]?.price_monthly?.gross;
    return truthy(gross) ? `${label} · €${numberFormat(gross)}/mo` : label;
  };

  async function submit(e: SubmitEvent) {
    e.preventDefault();
    busy = true;
    error = null;
    try {
      const { uuid } = await cloudUi.create('hetzner', tokenUuid as string, {
        server_name: serverName,
        selected_location: location,
        selected_image: intOrNull(image),
        selected_server_type: serverType,
        private_key_id: intOrNull(privateKey),
        selectedHetznerSshKeyIds: sshKeys,
        selectedHetznerFirewallIds: firewalls,
        selectedHetznerNetworkIds: networks,
        enable_ipv4: enableIpv4,
        enable_ipv6: enableIpv6,
        enable_backups: enableBackups,
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
  provider="hetzner"
  providerLabel="Hetzner"
  routeType="hetzner"
  {tokenUuid}
  onready={init}
  loadingDescription="Fetching locations, server types, images, and account resources."
  signupUrl="https://coolify.io/hetzner"
  signupText="through Coolify's affiliate link."
>
  {#snippet children(form: CloudForm, refreshKeys)}
    <form onsubmit={submit} class="flex flex-col gap-6">
      <SettingsSection title="Hetzner server" description="Choose the location, hardware, operating system, and Coolify SSH key.">
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
            id="selected_location"
            label="Location"
            required
            placeholder="Select a location"
            bind:value={location}
            onchange={locationChanged}
            error={error?.field('selected_location')}
            options={d.locations.map((l) => ({ value: l.name, label: `${l.city} · ${l.country}` }))}
          />
          <Listbox
            id="selected_server_type"
            label="Server type"
            required
            disabled={!location}
            placeholder="Select a server type"
            bind:value={serverType}
            onchange={() => (image = '')}
            error={error?.field('selected_server_type')}
            options={availableServerTypes.map((t) => ({ value: t.name, label: typeLabel(t) }))}
          />
          <Listbox
            id="selected_image"
            label="Image"
            required
            disabled={!serverType}
            placeholder="Select an image"
            bind:value={image}
            error={error?.field('selected_image')}
            options={availableImages.map((i) => ({
              value: String(i.id),
              label: `${i.description ?? i.name}${i.architecture !== null && i.architecture !== undefined ? ` · ${i.architecture}` : ''}`,
            }))}
          />
          <CloudPrivateKey keys={form.keys} bind:value={privateKey} helper="This key is added to the Hetzner server automatically." error={error?.field('private_key_id')} refresh={refreshKeys} />
        </div>
      </SettingsSection>

      <SettingsSection title="Advanced options" description="Provider SSH keys, networking, backups, and cloud-init.">
        {#if summary.length > 0}
          <div class="mb-4 flex flex-wrap gap-1.5">
            {#each summary as item (item)}
              <span class="rounded-full bg-neutral-100 px-2 py-0.5 text-[10px] font-medium text-neutral-600 dark:bg-white/[0.06] dark:text-fg-dim">{item}</span>
            {/each}
          </div>
        {/if}
        <div class="flex flex-col gap-4">
          <Listbox
            id="selectedHetznerSshKeyIds"
            label="Extra SSH keys"
            helper="Existing keys from the Hetzner account."
            multiple
            searchable
            disabled={d.ssh_keys.length === 0}
            searchPlaceholder="Search SSH keys…"
            placeholder={d.ssh_keys.length ? 'Select SSH keys' : 'No account keys found'}
            bind:values={sshKeys}
            error={error?.field('selectedHetznerSshKeyIds')}
            options={d.ssh_keys.map((k) => ({ value: k.id, label: k.name }))}
          />
          <div class="grid gap-4 lg:grid-cols-2">
            <Listbox
              id="selectedHetznerFirewallIds"
              label="Firewalls"
              multiple
              searchable
              disabled={d.firewalls.length === 0}
              searchPlaceholder="Search firewalls…"
              placeholder={d.firewalls.length ? 'Select firewalls' : 'No firewalls found'}
              bind:values={firewalls}
              error={error?.field('selectedHetznerFirewallIds')}
              options={d.firewalls.map((f) => ({ value: f.id, label: f.name }))}
            />
            <Listbox
              id="selectedHetznerNetworkIds"
              label="Private networks"
              multiple
              searchable
              disabled={availableNetworks.length === 0}
              searchPlaceholder="Search networks…"
              placeholder={availableNetworks.length ? 'Select networks' : 'No compatible networks'}
              bind:values={networks}
              error={error?.field('selectedHetznerNetworkIds')}
              options={availableNetworks.map((n) => ({ value: n.id, label: n.name, description: n.ip_range }))}
            />
          </div>
          <div class="grid gap-3 lg:grid-cols-3">
            <Checkbox id="enable_ipv4" label="Enable IPv4" fullWidth bind:checked={enableIpv4} />
            <Checkbox id="enable_ipv6" label="Enable IPv6" fullWidth bind:checked={enableIpv6} />
            <Checkbox id="enable_backups" label="Enable Hetzner backups" fullWidth helper="Adds 20% to the provider server price." bind:checked={enableBackups} />
          </div>
          <CloudInitFields scripts={form.scripts} bind:show={showScript} bind:script bind:selectedId={scriptId} bind:save={saveScript} bind:name={scriptName} {error} />
        </div>
      </SettingsSection>
    </form>
  {/snippet}
</CloudProvision>
