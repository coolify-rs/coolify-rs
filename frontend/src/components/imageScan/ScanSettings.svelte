<script lang="ts">
  // Port of upstream livewire/image-scan/scan-settings.blade.php with partials/scan-rules and
  // partials/scan-ignores (ImageScan\ScanSettings): the scan rules of one level, saved on change,
  // and the ignores of that level. Databases and services only list their ignores.
  import { untrack } from 'svelte';
  import ConfirmModal from '../ConfirmModal.svelte';
  import Empty from '../Empty.svelte';
  import InheritedListbox from '../InheritedListbox.svelte';
  import Listbox from '../Listbox.svelte';
  import SettingsSection from '../SettingsSection.svelte';
  import SettingsSubsection from '../SettingsSubsection.svelte';
  import { link } from '../../lib/router.svelte';
  import { run, scanSettingsApi as api, type ScanSettingsData } from '../../lib/imageScanApi';

  let {
    scope,
    uuid = null,
    embedded = false,
    onsaved,
  }: {
    scope: 'instance' | 'team' | 'server' | 'application' | 'database' | 'service';
    uuid?: string | null;
    /** Renders the rules without their own card, for a page that puts them inside another card. */
    embedded?: boolean;
    onsaved?: () => void;
  } = $props();

  let data = $state<ScanSettingsData | null>(null);
  let values = $state({ gate_mode: '', on_scan_error: '', count_medium: '', app_scan_enabled: '1', builds_by_team: '', scan_new_servers: '0' });
  let removing = $state<string | null>(null);

  async function load() {
    const r = await api.get(scope, uuid);
    data = r;
    if (r.values) values = { ...r.values };
  }

  $effect(() => {
    void scope;
    void uuid;
    untrack(() => void load().catch(() => {}));
  });

  /** `updated{Property}`: saves one field; the page shows the stored values again after it. */
  async function save(field: keyof typeof values, value: string) {
    values[field] = value;
    await run(() => api.save(scope, uuid, field, value));
    await load().catch(() => {});
    onsaved?.();
  }

  const isApplication = $derived(scope === 'application');
  const ignoresOnly = $derived(scope === 'database' || scope === 'service');
  const isResource = $derived(isApplication || ignoresOnly);
  const rulesHelper = $derived(
    {
      instance: 'Defaults for every team. A team, server, or app can set its own value.',
      team: 'Defaults for the servers and apps of this team. A server or app can set its own value.',
      server: 'Defaults for the apps on this server. An app can set its own value.',
    }[scope as string] ?? 'Rules for this app.',
  );
  const gateHelper = $derived(isApplication ? 'Stop a deploy when the new image has security problems.' : 'Default for apps. Stop a deploy when the new image has security problems.');
  const ignoresHelper =
    'Ignored findings do not count for the status, alerts, or Block deploys. Ignore a finding from All findings on the Security page of a resource.';
  const onOff = [
    { value: '0', label: 'Off' },
    { value: '1', label: 'On' },
  ];
</script>

{#snippet rules(d: ScanSettingsData)}
  <div class="flex flex-col gap-4">
    {#if isApplication || scope === 'team'}
      <div class="grid gap-4 md:grid-cols-2 lg:grid-cols-3">
        {#if isApplication}
          <Listbox
            id="appScanEnabled"
            label="Scan this app"
            disabled={!d.can_edit}
            helper="Off stops the scans of this app's image."
            value={values.app_scan_enabled}
            onchange={(v) => save('app_scan_enabled', String(v))}
            options={[
              { value: '1', label: 'On' },
              { value: '0', label: 'Off' },
            ]}
          />
          {#if d.is_docker_image}
            <Listbox
              id="buildsByTeam"
              label="Who builds this image"
              disabled={!d.can_edit}
              helper="Coolify detects whether your team builds this image. Set it when the detection is wrong."
              value={values.builds_by_team}
              onchange={(v) => save('builds_by_team', String(v))}
              options={[
                { value: '', label: 'Detect automatically' },
                { value: '1', label: 'My team' },
                { value: '0', label: 'Someone else' },
              ]}
            />
          {/if}
        {:else}
          <Listbox
            id="scanNewServers"
            label="Scan images on new servers"
            disabled={!d.can_edit}
            helper="Pre-selects image scanning when someone adds a server to this team. They can change it."
            value={values.scan_new_servers}
            onchange={(v) => save('scan_new_servers', String(v))}
            options={onOff}
          />
        {/if}
      </div>
    {/if}

    <div class="grid gap-4 md:grid-cols-2 lg:grid-cols-3">
      <div class="min-w-0">
        <InheritedListbox
          id="gateMode"
          label="Block deploys"
          disabled={!d.can_edit || !d.app_scan_on}
          inheritLabel={d.inherit_labels?.gate_mode ?? null}
          options={d.gate_mode_options}
          helper={gateHelper}
          value={values.gate_mode}
          onchange={(v) => save('gate_mode', v)}
        />
        {#if !d.app_scan_on}
          <p class="mt-1.5 text-[12px] text-neutral-500 dark:text-fg-faint">Turn on the scan for this app first.</p>
        {/if}
      </div>
      <InheritedListbox
        id="onScanError"
        label="If the scan cannot run"
        disabled={!d.can_edit}
        inheritLabel={d.inherit_labels?.on_scan_error ?? null}
        options={d.on_scan_error_options}
        helper="Applies when Block deploys is On or Strict."
        value={values.on_scan_error}
        onchange={(v) => save('on_scan_error', v)}
      />
      <InheritedListbox
        id="countMedium"
        label="Count medium problems"
        disabled={!d.can_edit}
        inheritLabel={d.inherit_labels?.count_medium ?? null}
        options={d.count_medium_options}
        helper="Medium problems that an update fixes give a security update. Problems that attackers are likely to use always count."
        value={values.count_medium}
        onchange={(v) => save('count_medium', v)}
      />
    </div>

    {#if scope === 'instance' || scope === 'team' || scope === 'server'}
      <p class="text-[12px] text-neutral-500 dark:text-fg-faint">Block deploys applies only to apps.</p>
    {/if}
  </div>
{/snippet}

{#snippet ignores(d: ScanSettingsData)}
  {#if d.ignores.length === 0}
    <Empty size="sm" title="No ignored findings." />
  {:else}
    <div class="data-table">
      <div class="data-table-header image-scan-ignores-table-grid">
        <span>Finding or package</span>
        <span>Reason</span>
        <span>Expires</span>
        <span>Set for</span>
        <span class="text-right">Actions</span>
      </div>
      {#each d.ignores as ignore (ignore.uuid)}
        <div class="data-table-row image-scan-ignores-table-grid border-b border-neutral-200 last:border-b-0 dark:border-white/[0.07]">
          <div class="min-w-0 truncate text-[12px] text-black dark:text-fg">
            {#if ignore.vulnerability_id}
              <span class="font-mono">{ignore.vulnerability_id}</span>
            {:else}
              All findings in <span class="font-mono">{ignore.package}</span>
            {/if}
          </div>
          <div class="min-w-0 truncate text-[12px] text-neutral-600 dark:text-fg-dim" title={ignore.reason}>{ignore.reason}</div>
          <div class="text-[11px] text-neutral-500 dark:text-fg-dim">
            {ignore.expires ?? 'Never'}
          </div>
          <div class="min-w-0 text-[11px] text-neutral-500 dark:text-fg-dim">
            {ignore.team_wide ? 'All team resources' : (ignore.set_for ?? 'This resource')}
          </div>
          <div class="flex justify-end">
            {#if isResource && ignore.team_wide}
              {#if d.shows_team_settings_link}
                <a
                  href="/team/image-scanning"
                  use:link
                  class="inline-flex h-7 items-center rounded-md px-2 text-[11px] font-medium text-neutral-600 hover:bg-neutral-100 dark:text-fg-dim dark:hover:bg-white/[0.06]"
                >
                  Team settings
                </a>
              {/if}
            {:else if d.can_edit}
              <button
                type="button"
                class="inline-flex h-7 items-center rounded-md px-2 text-[11px] font-medium text-error transition-colors hover:bg-error/10"
                onclick={() => (removing = ignore.uuid)}
              >
                Remove
              </button>
            {/if}
          </div>
        </div>
      {/each}
    </div>
  {/if}
{/snippet}

{#if data}
  <div class="flex flex-col gap-6">
    {#if ignoresOnly}
      <SettingsSection id="image-scan-ignores-section" title="Ignored findings" description={ignoresHelper} collapsible flush>
        {@render ignores(data)}
      </SettingsSection>
    {:else if isApplication}
      <SettingsSection id="image-scan-settings-section" title="Settings" collapsible open={!data.app_scan_on}>
        <SettingsSubsection title="Rules" helper={rulesHelper} first>
          {@render rules(data)}
        </SettingsSubsection>
        {#if data.shows_ignores}
          <SettingsSubsection title="Ignored findings" helper={ignoresHelper}>
            <div class="overflow-hidden rounded-lg border border-neutral-200 dark:border-white/[0.08]">
              {@render ignores(data)}
            </div>
          </SettingsSubsection>
        {/if}
      </SettingsSection>
    {:else if embedded}
      <SettingsSubsection id="image-scan-rules-section" title="Rules" helper={rulesHelper} first>
        {@render rules(data)}
      </SettingsSubsection>
    {:else}
      <SettingsSection id="image-scan-rules-section" title="Rules" description={rulesHelper}>
        {@render rules(data)}
      </SettingsSection>

      {#if data.shows_ignores}
        <SettingsSection id="image-scan-ignores-section" title="Ignored findings" description={ignoresHelper} flush>
          {@render ignores(data)}
        </SettingsSection>
      {/if}
    {/if}
  </div>
{/if}

{#if removing}
  <ConfirmModal
    title="Remove this ignore?"
    actions={['The finding counts again for the status, alerts, and Block deploys.']}
    confirmWithText={false}
    confirmWithPassword={false}
    step2ButtonText="Remove"
    onconfirm={async () => {
      const id = removing;
      if (id && (await run(() => api.removeIgnore(scope, uuid, id)))) {
        await load().catch(() => {});
        onsaved?.();
      }
    }}
    onclose={() => (removing = null)}
  />
{/if}
