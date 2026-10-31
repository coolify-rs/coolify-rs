<script lang="ts">
  // Port of upstream livewire/server/new/by-hostinger.blade.php (`Server\New\ByHostinger`).
  import Callout from '../components/Callout.svelte';
  import Checkbox from '../components/Checkbox.svelte';
  import CloudPrivateKey from '../components/CloudPrivateKey.svelte';
  import CloudProvision from '../components/CloudProvision.svelte';
  import FormInput from '../components/FormInput.svelte';
  import Listbox from '../components/Listbox.svelte';
  import SettingsSection from '../components/SettingsSection.svelte';
  import { ApiError } from '../lib/api';
  import { cloudUi, intOrNull, plural, truthy, type CloudForm, type Item } from '../lib/cloudApi';
  import { failure } from '../lib/serverApi';
  import { router } from '../lib/router.svelte';

  type Data = { data_centers: Item[]; templates: Item[]; catalog_items: Item[]; public_keys: Item[]; post_install_scripts: Item[] };
  let { tokenUuid }: { tokenUuid?: string } = $props();

  let d = $state<Data>({ data_centers: [], templates: [], catalog_items: [], public_keys: [], post_install_scripts: [] });
  let serverName = $state('');
  let dataCenter = $state('');
  let priceId = $state('');
  let osTemplate = $state('');
  let appTemplate = $state('');
  let templateId = $state('');
  let privateKey = $state('');
  let enableBackups = $state(false);
  let publicKeys = $state<number[]>([]);
  let postInstallScript = $state('');
  let error = $state<ApiError | null>(null);
  let busy = $state(false);

  /** `templateIncludesApplication`. */
  const includesApplication = (t: Item) => String(t.name ?? '').includes(' with ');
  const osTemplates = $derived(d.templates.filter((t) => !includesApplication(t)));
  const appTemplates = $derived(d.templates.filter(includesApplication));

  function init(form: CloudForm, data: Data) {
    d = data;
    serverName = form.name;
    if (form.keys.length) privateKey = String(form.keys[0].id);
    // `selectDefaultTemplate`.
    const fallback = data.templates.find((t) => !includesApplication(t) && t.name === 'Ubuntu 24.04 LTS');
    if (fallback) {
      osTemplate = String(fallback.id);
      templateId = osTemplate;
    }
  }

  /** `planSize`: hostingercom-vps-kvm4 is 4, otherwise its CPU count. */
  function planSize(item: Item): number {
    const m = /-vps-kvm(\d+)$/.exec(String(item.id ?? ''));
    return m ? Number(m[1]) : Math.trunc(Number(item.metadata?.cpus ?? 0));
  }

  /** `periodInMonths`. */
  function periodInMonths(p: Item): number {
    const period = Math.trunc(Number(p.period ?? 0));
    switch (p.period_unit ?? 'month') {
      case 'day':
        return period / 30;
      case 'week':
        return (period * 7) / 30;
      case 'year':
        return period * 12;
      case 'none':
        return Number.MAX_SAFE_INTEGER;
      default:
        return period;
    }
  }

  /** `getPriceOptionsProperty`. */
  const priceOptions = $derived(
    d.catalog_items
      .flatMap((item) => (item.prices ?? []).map((p: Item) => ({ ...p, plan_name: item.name ?? item.id ?? 'VPS', plan_size: planSize(item) })))
      .sort((a, b) => a.plan_size - b.plan_size || periodInMonths(a) - periodInMonths(b)),
  );

  /** `priceLabel`. */
  function priceLabel(p: Item): string {
    const currency = String(p.currency ?? 'USD').toUpperCase();
    const first = Math.trunc(Number(p.first_period_price ?? p.price ?? 0));
    const renewal = Math.trunc(Number(p.price ?? first));
    const period = Math.trunc(Number(p.period ?? 1));
    const label = `${period} ${plural(p.period_unit ?? 'month', period)}`;
    return `${p.plan_name ?? 'VPS'} — ${currency} ${(first / 100).toFixed(2)} for ${label}, then ${currency} ${(renewal / 100).toFixed(2)}`;
  }

  const templateOption = (t: Item) => ({ value: String(t.id), label: t.name ?? t.description ?? String(t.id) });

  /** `updatedSelectedOsTemplateId`. */
  function osChanged() {
    if (osTemplate !== '') appTemplate = '';
    templateId = osTemplate !== '' ? osTemplate : appTemplate;
  }

  /** `updatedSelectedAppTemplateId`. */
  function appChanged() {
    if (appTemplate !== '') osTemplate = '';
    templateId = appTemplate !== '' ? appTemplate : osTemplate;
  }

  async function submit(e: SubmitEvent) {
    e.preventDefault();
    busy = true;
    error = null;
    try {
      const { uuid } = await cloudUi.create('hostinger', tokenUuid as string, {
        server_name: serverName,
        selected_data_center_id: intOrNull(dataCenter),
        selected_template_id: intOrNull(templateId),
        selected_price_id: priceId,
        private_key_id: intOrNull(privateKey),
        enable_backups: enableBackups,
        selected_public_key_ids: publicKeys,
        selected_post_install_script_id: intOrNull(postInstallScript),
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
  provider="hostinger"
  providerLabel="Hostinger"
  routeType="hostinger"
  {tokenUuid}
  onready={init}
  spinner
  loadingDescription="Fetching data centers, plans, billing periods, and operating systems."
  signupUrl="https://www.hostinger.com/vps/coolify-hosting?ref=coolify.io"
  signupRel="noopener noreferrer"
  signupText="through Coolify's affiliate link."
>
  {#snippet children(form: CloudForm, refreshKeys)}
    <form onsubmit={submit} class="flex flex-col gap-6">
      <SettingsSection title="Hostinger server" description="Choose the data center, plan, operating system, and Coolify SSH key.">
        {#snippet actions()}
          <button type="submit" class="button button-highlighted" disabled={!privateKey || busy}>
            Buy and create
            {#if busy}
              <span class="inline-flex shrink-0 items-center gap-1.5">
                <svg class="size-3.5 shrink-0 animate-spin" viewBox="0 0 24 24" fill="none" aria-hidden="true">
                  <circle class="opacity-25" cx="12" cy="12" r="9" stroke="currentColor" stroke-width="3" />
                  <path class="opacity-75" d="M21 12a9 9 0 0 0-9-9" stroke="currentColor" stroke-width="3" stroke-linecap="round" />
                </svg>
              </span>
            {/if}
          </button>
        {/snippet}
        <div class="grid gap-4 lg:grid-cols-2">
          <div class="lg:col-span-2">
            <FormInput id="server_name" label="Server name" helper="A friendly name shown in Coolify and used as the VPS hostname." bind:value={serverName} error={error?.field('server_name')} />
          </div>
          <Listbox
            id="selected_data_center_id"
            label="Data center"
            required
            placeholder="Select a data center"
            bind:value={dataCenter}
            error={error?.field('selected_data_center_id')}
            options={d.data_centers.map((c) => ({
              value: String(c.id),
              label: `${c.city ?? c.name ?? c.id}${truthy(c.location) ? ` · ${String(c.location).toUpperCase()}` : ''}`,
            }))}
          />
          <Listbox
            id="selected_price_id"
            label="Plan and billing period"
            required
            disabled={!dataCenter}
            placeholder="Select a plan"
            bind:value={priceId}
            error={error?.field('selected_price_id')}
            options={priceOptions.map((p) => ({ value: p.id, label: priceLabel(p) }))}
          />
          <Listbox
            id="selected_os_template_id"
            label="Operating system"
            required
            disabled={!priceId}
            placeholder="Select an operating system"
            bind:value={osTemplate}
            onchange={osChanged}
            error={error?.field('selected_os_template_id')}
            options={osTemplates.map(templateOption)}
          />
          <Listbox
            id="selected_app_template_id"
            label="Operating system with application"
            disabled={!priceId}
            placeholder="Or select an operating system with an application"
            helper="Replaces the operating system selected above. Applications and panels, such as another Coolify or CyberPanel, can conflict with the Coolify proxy on ports 80 and 443."
            bind:value={appTemplate}
            onchange={appChanged}
            error={error?.field('selected_app_template_id')}
            options={appTemplates.map(templateOption)}
          />
          <CloudPrivateKey keys={form.keys} bind:value={privateKey} helper="This key is added to the Hostinger VPS automatically." error={error?.field('private_key_id')} refresh={refreshKeys} />
        </div>
      </SettingsSection>

      <SettingsSection title="Advanced options" description="Provider backups, account SSH keys, and post-install automation.">
        <div class="flex flex-col gap-4">
          <div class="-mx-2.5 grid gap-3 lg:grid-cols-2">
            <Checkbox
              id="enable_backups"
              label="Enable weekly Hostinger backups (extra cost)"
              fullWidth
              helper="Hostinger charges extra for backups. Check the price in hPanel before you enable them."
              bind:checked={enableBackups}
            />
          </div>
          <Listbox
            id="selected_public_key_ids"
            label="Extra SSH keys"
            helper="Existing keys from the Hostinger account."
            multiple
            searchable
            disabled={d.public_keys.length === 0}
            searchPlaceholder="Search SSH keys…"
            placeholder={d.public_keys.length ? 'Select SSH keys' : 'No account keys found'}
            bind:values={publicKeys}
            error={error?.field('selected_public_key_ids')}
            options={d.public_keys.map((k) => ({ value: k.id, label: k.name }))}
          />
          {#if d.post_install_scripts.length > 0}
            <Listbox
              id="selected_post_install_script_id"
              label="Hostinger post-install script"
              placeholder="No post-install script"
              helper="Hostinger runs this as /post_install after setup and writes output to /post_install.log."
              bind:value={postInstallScript}
              error={error?.field('selected_post_install_script_id')}
              options={d.post_install_scripts.map((s) => ({ value: String(s.id), label: s.name }))}
            />
          {/if}
          <Callout type="warning" title="This purchase is billed by Hostinger">
            The VPS is purchased with your Hostinger account's default payment method. Review the
            selected plan and billing period before continuing.
          </Callout>
        </div>
      </SettingsSection>
    </form>
  {/snippet}
</CloudProvision>
