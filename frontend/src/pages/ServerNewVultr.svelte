<script lang="ts">
  // Port of upstream livewire/server/new/by-vultr.blade.php (`Server\New\ByVultr`).
  import Checkbox from '../components/Checkbox.svelte';
  import CloudInitFields from '../components/CloudInitFields.svelte';
  import CloudPrivateKey from '../components/CloudPrivateKey.svelte';
  import CloudProvision from '../components/CloudProvision.svelte';
  import FormInput from '../components/FormInput.svelte';
  import Listbox from '../components/Listbox.svelte';
  import SettingsSection from '../components/SettingsSection.svelte';
  import { ApiError } from '../lib/api';
  import { cloudUi, intOrNull, numberFormat, plural, type CloudForm, type Item } from '../lib/cloudApi';
  import { failure } from '../lib/serverApi';
  import { router } from '../lib/router.svelte';

  type Data = { regions: Item[]; plans: Item[]; operating_systems: Item[]; ssh_keys: Item[] };
  let { tokenUuid }: { tokenUuid?: string } = $props();

  let d = $state<Data>({ regions: [], plans: [], operating_systems: [], ssh_keys: [] });
  let serverName = $state('');
  let region = $state('');
  let plan = $state('');
  let osId = $state('');
  let privateKey = $state('');
  let sshKeys = $state<string[]>([]);
  let enableIpv6 = $state(true);
  let disablePublicIpv4 = $state(false);
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

  /** `getAvailablePlansProperty`. */
  const availablePlans = $derived(
    region
      ? d.plans.filter((p) => {
          const locations = p.locations ?? [];
          return locations.length === 0 || locations.includes(region);
        })
      : d.plans,
  );

  /** `getSelectedServerPriceProperty`. */
  const price = $derived.by(() => {
    const cost = d.plans.find((p) => p.id === plan)?.monthly_cost;
    return plan && cost !== null && cost !== undefined ? `$${numberFormat(cost)}` : null;
  });

  /** `getAdvancedVultrOptionsSummaryProperty`. */
  const summary = $derived.by(() => {
    const out: string[] = [];
    if (sshKeys.length > 0) out.push(`${sshKeys.length} extra SSH ${plural('key', sshKeys.length)}`);
    if (!enableIpv6) out.push('IPv6 disabled');
    if (disablePublicIpv4) out.push('Public IPv4 disabled');
    if (script && script !== '0') out.push('cloud-init');
    return out;
  });

  const planLabel = (p: Item) => {
    const ram = p.ram !== null && p.ram !== undefined ? numberFormat(p.ram / 1024, 1) : '?';
    const label = `${p.id} · ${p.vcpu_count ?? '?'} vCPU · ${ram} GB RAM · ${p.disk ?? '?'} GB`;
    return p.monthly_cost !== null && p.monthly_cost !== undefined ? `${label} · $${numberFormat(p.monthly_cost)}/mo` : label;
  };

  async function submit(e: SubmitEvent) {
    e.preventDefault();
    busy = true;
    error = null;
    try {
      const { uuid } = await cloudUi.create('vultr', tokenUuid as string, {
        server_name: serverName,
        selected_region: region,
        selected_plan: plan,
        selected_os_id: intOrNull(osId),
        private_key_id: intOrNull(privateKey),
        selectedVultrSshKeyIds: sshKeys,
        enable_ipv6: enableIpv6,
        disable_public_ipv4: disablePublicIpv4,
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
  provider="vultr"
  providerLabel="Vultr"
  routeType="vultr"
  {tokenUuid}
  onready={init}
  loadingDescription="Fetching regions, plans, operating systems, and account resources."
  signupUrl="https://coolify.io/vultr"
  signupText="through Coolify's affiliate link."
>
  {#snippet children(form: CloudForm, refreshKeys)}
    <form onsubmit={submit} class="flex flex-col gap-6">
      <SettingsSection title="Vultr server" description="Choose the region, plan, operating system, and coolify-rs SSH key.">
        {#snippet actions()}
          <button type="submit" class="button button-highlighted" disabled={!privateKey || busy}>
            Buy and create
            {#if price}<span class="opacity-70">· {price}/mo</span>{/if}
          </button>
        {/snippet}
        <div class="grid gap-4 lg:grid-cols-2">
          <div class="lg:col-span-2">
            <FormInput id="server_name" label="Server name" helper="A friendly name shown in coolify-rs." bind:value={serverName} error={error?.field('server_name')} />
          </div>
          <Listbox
            id="selected_region"
            label="Region"
            required
            placeholder="Select a region"
            bind:value={region}
            onchange={() => (plan = '')}
            error={error?.field('selected_region')}
            options={d.regions.map((r) => ({ value: r.id, label: `${r.city ?? r.id} · ${r.country ?? r.id}` }))}
          />
          <Listbox
            id="selected_plan"
            label="Plan"
            required
            disabled={!region}
            placeholder="Select a plan"
            bind:value={plan}
            error={error?.field('selected_plan')}
            options={availablePlans.map((p) => ({ value: p.id, label: planLabel(p) }))}
          />
          <Listbox
            id="selected_os_id"
            label="Operating system"
            required
            placeholder="Select an operating system"
            bind:value={osId}
            error={error?.field('selected_os_id')}
            options={d.operating_systems.map((o) => ({ value: String(o.id), label: o.name }))}
          />
          <CloudPrivateKey keys={form.keys} bind:value={privateKey} helper="This key is added to the Vultr server automatically." error={error?.field('private_key_id')} refresh={refreshKeys} />
        </div>
      </SettingsSection>

      <SettingsSection title="Advanced options" description="Provider SSH keys, networking, and cloud-init.">
        {#if summary.length > 0}
          <div class="mb-4 flex flex-wrap gap-1.5">
            {#each summary as item (item)}
              <span class="rounded-full bg-neutral-100 px-2 py-0.5 text-[10px] font-medium text-neutral-600 dark:bg-white/[0.06] dark:text-fg-dim">{item}</span>
            {/each}
          </div>
        {/if}
        <div class="flex flex-col gap-4">
          <Listbox
            id="selectedVultrSshKeyIds"
            label="Extra SSH keys"
            helper="Existing keys from the Vultr account."
            multiple
            searchable
            disabled={d.ssh_keys.length === 0}
            searchPlaceholder="Search SSH keys…"
            placeholder={d.ssh_keys.length ? 'Select SSH keys' : 'No account keys found'}
            bind:values={sshKeys}
            error={error?.field('selectedVultrSshKeyIds')}
            options={d.ssh_keys.map((k) => ({ value: k.id, label: k.name }))}
          />
          <div class="grid gap-3 lg:grid-cols-2">
            <Checkbox id="enable_ipv6" label="Enable IPv6" fullWidth bind:checked={enableIpv6} />
            <Checkbox id="disable_public_ipv4" label="Disable public IPv4" fullWidth bind:checked={disablePublicIpv4} />
          </div>
          <CloudInitFields scripts={form.scripts} alwaysOpen bind:script bind:selectedId={scriptId} bind:save={saveScript} bind:name={scriptName} {error} />
        </div>
      </SettingsSection>
    </form>
  {/snippet}
</CloudProvision>
