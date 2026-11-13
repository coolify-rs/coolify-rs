<script lang="ts">
  // Port of upstream livewire/server/security/patches.blade.php (v4.7.5: search, pagination,
  // selection and the split-action menu).
  import { onDestroy } from 'svelte';
  import BetaBadge from '../components/BetaBadge.svelte';
  import Callout from '../components/Callout.svelte';
  import ConfirmModal from '../components/ConfirmModal.svelte';
  import Empty from '../components/Empty.svelte';
  import FormInput from '../components/FormInput.svelte';
  import Helper from '../components/Helper.svelte';
  import Pager from '../components/Pager.svelte';
  import Reicon from '../components/Reicon.svelte';
  import RemoveUnusedPackagesButton, { removeUnusedSupported } from '../components/RemoveUnusedPackagesButton.svelte';
  import ServerActivityDialog from '../components/ServerActivityDialog.svelte';
  import ServerFrame from '../components/ServerFrame.svelte';
  import SettingsSection from '../components/SettingsSection.svelte';
  import StatusBadge from '../components/StatusBadge.svelte';
  import UnsavedBar from '../components/UnsavedBar.svelte';
  import { diffForHumans } from '../lib/format';
  import { link } from '../lib/router.svelte';
  import { get, post, failure, action } from '../lib/serverApi';
  import { session } from '../lib/session.svelte';

  let { uuid }: { uuid: string } = $props();

  type Update = { package: string; architecture?: string; new_version?: string; current_version?: string; security?: boolean; risk?: string | null; held_reason?: string };
  type Result = { updates?: Update[]; held_updates?: Update[]; package_manager?: string | null; security_updates?: number | null; reboot_required?: boolean | null } | null;
  type Data = {
    name: string;
    patch_check_frequency: string;
    auto_security_updates: boolean;
    auto_security_updates_time: string;
    result: Result;
    error: string | null;
    checked_at: string | null;
    checking: boolean;
    confirm_with_text: boolean;
  };

  // CheckUpdates::RISK_LABELS, HELD_REASON_LABELS, FULL_UPGRADE_ONLY
  const RISK_LABELS: Record<string, string> = {
    docker: 'Restarts Docker and the running containers',
    kernel: 'Needs a reboot to apply',
    system: 'Restarts core system services',
  };
  const HELD_REASON_LABELS: Record<string, string> = { phased: 'Phased rollout', held: 'Held' };
  /** `Patches::SEARCH_MAX_LENGTH`: package names are short, so a longer search can match nothing. */
  const SEARCH_MAX_LENGTH = 100;

  let data = $state<Data | null>(null);
  let form = $state({ frequency: '', time: '03:00' });
  let saved = $state({ frequency: '', time: '03:00' });
  let saving = $state(false);
  let modal = $state<'reboot' | 'auto' | 'all' | 'selected' | null>(null);
  let removeUnusedOpen = $state(false);
  let menuOpen = $state(false);
  let menuRoot: HTMLDivElement | undefined = $state();
  let dialog = $state(false);
  let activityId = $state<number | null>(null);
  let search = $state('');
  let page = $state(1);
  let perPage = $state(10);
  /** Packages of the last check that the user selected to update. */
  let selectedPackages = $state<string[]>([]);
  let poll: ReturnType<typeof setInterval> | undefined;

  const url = $derived(`/server-page/${encodeURIComponent(uuid)}/security/patches`);
  const canEdit = $derived(session.isAdmin);
  const dirty = $derived(form.frequency !== saved.frequency || form.time !== saved.time);
  const plural = (word: string, n: number) => (n === 1 ? word : `${word}s`);
  const cmp = (a: string, b: string) => (a < b ? -1 : a > b ? 1 : 0);

  const updates = $derived(
    [...(data?.result?.updates ?? [])].sort(
      (a, b) => (a.security ? 0 : 1) - (b.security ? 0 : 1) || (a.risk ? 0 : 1) - (b.risk ? 0 : 1) || cmp(a.package, b.package),
    ),
  );
  const totalUpdates = $derived(updates.length);
  const heldUpdates = $derived([...(data?.result?.held_updates ?? [])].sort((a, b) => cmp(a.package, b.package)));
  const packageManager = $derived(data?.result?.package_manager ?? null);
  const fullUpgradeOnly = $derived(packageManager === 'pacman');
  const securityUpdates = $derived(data?.result?.security_updates ?? null);
  const rebootRequired = $derived(data?.result?.reboot_required === true);
  const riskyUpdates = $derived(updates.filter((u) => u.risk).length);
  const autoUpdatesSupported = $derived(packageManager === 'apt' || packageManager === 'dnf');
  const checkedAtText = $derived(data?.checked_at ? data.checked_at.replace('T', ' ').slice(0, 19) : '');
  const canSelect = $derived(!fullUpgradeOnly && canEdit);

  /** `str($package)->contains($search, ignoreCase: true)`. */
  const matches = (name: string, term: string) => term === '' || name.toLowerCase().includes(term.toLowerCase());
  const matchingUpdates = $derived(updates.filter((u) => matches(u.package, search.trim())));
  const lastPage = $derived(Math.max(1, Math.ceil(matchingUpdates.length / perPage)));
  const shownUpdates = $derived(matchingUpdates.slice((Math.min(page, lastPage) - 1) * perPage, Math.min(page, lastPage) * perPage));
  const allMatchingSelected = $derived(matchingUpdates.length > 0 && matchingUpdates.every((u) => selectedPackages.includes(u.package)));
  const selectedRisky = $derived(updates.filter((u) => selectedPackages.includes(u.package) && u.risk).length);

  // `render()`: the page stays within the matching updates.
  $effect(() => {
    if (page > lastPage) page = lastPage;
  });

  /** `savedPackages()`. */
  const savedPackages = () => (data?.result?.updates ?? []).map((u) => u.package);

  /** `loadResult`: everything but the form. */
  async function loadResult() {
    const d = await get<Data>(url).catch(() => null);
    if (d && data) {
      data.result = d.result;
      data.error = d.error;
      data.checked_at = d.checked_at;
      data.checking = d.checking;
      data.auto_security_updates = d.auto_security_updates;
      const saved = savedPackages();
      selectedPackages = selectedPackages.filter((p) => saved.includes(p));
    }
  }
  /** `syncSettings` and `loadResult`. */
  async function load() {
    const d = await get<Data>(url);
    data = d;
    form = { frequency: d.patch_check_frequency, time: d.auto_security_updates_time };
    saved = { ...form };
    const known = savedPackages();
    selectedPackages = selectedPackages.filter((p) => known.includes(p));
    clearInterval(poll);
    // `wire:poll.10s` while checking, and the `ServerPatchCheckFinished` broadcast.
    poll = setInterval(loadResult, 10000);
  }
  onDestroy(() => clearInterval(poll));

  /** `updatedSearch`. */
  function searchChanged() {
    search = search.slice(0, SEARCH_MAX_LENGTH);
    page = 1;
  }

  /** `toggleAllPackages`: selects all packages that match the search, or clears the selection when they are all selected. */
  function toggleAllPackages() {
    const term = search.trim();
    const matching = savedPackages().filter((p) => matches(p, term));
    selectedPackages = matching.every((p) => selectedPackages.includes(p))
      ? selectedPackages.filter((p) => !matching.includes(p))
      : [...new Set([...selectedPackages, ...matching])];
  }

  function togglePackage(name: string, checked: boolean) {
    selectedPackages = checked ? [...new Set([...selectedPackages, name])] : selectedPackages.filter((p) => p !== name);
  }

  async function save() {
    saving = true;
    await action(() => post(url, { patchCheckFrequency: form.frequency, autoSecurityUpdatesTime: form.time }));
    saving = false;
    await load();
  }
  async function check() {
    try {
      await post(`${url}/check`);
      if (data) data.checking = true;
    } catch (e) {
      failure(e);
    }
  }
  /** Returns true when the update started (`dispatch('startupdate')`). */
  async function start(path: string, body: unknown = {}): Promise<boolean> {
    try {
      const r = await post<{ activity_id: number }>(path, body);
      activityId = r.activity_id;
      dialog = true;
      return true;
    } catch (e) {
      failure(e);
      return false;
    }
  }
  /** `updateSelectedPackages`: the selection is cleared when the update started. */
  async function updateSelected() {
    if (await start(`${url}/update-selected`, { packages: selectedPackages })) selectedPackages = [];
  }
  async function toggleAuto() {
    await action(() => post(`${url}/auto-updates`, { autoSecurityUpdatesTime: form.time }));
    await load();
  }
  async function reboot() {
    await action(() => post(`${url}/reboot`));
  }
</script>

<svelte:window
  onclick={(e) => menuOpen && menuRoot && !menuRoot.contains(e.target as Node) && (menuOpen = false)}
  onkeydown={(e) => e.key === 'Escape' && (menuOpen = false)}
/>

{#snippet badges()}
  {#if data?.checking}
    <StatusBadge label="Checking…" type="neutral" spinning />
  {:else if data?.error}
    <StatusBadge label="Check failed" type="error" />
  {:else if data?.checked_at}
    {#if totalUpdates === 0}
      <StatusBadge label="Up to date" type="success" />
    {:else if securityUpdates !== null && securityUpdates > 0}
      <StatusBadge label="{securityUpdates} {plural('security update', securityUpdates)}" type="error" />
    {:else}
      <StatusBadge label="{totalUpdates} {plural('update', totalUpdates)}" type="warning" />
    {/if}
    {#if rebootRequired}
      <StatusBadge label="Reboot required" type="warning" />
    {/if}
  {:else}
    <StatusBadge label="Not checked yet" type="neutral" />
  {/if}
{/snippet}

{#snippet actions()}
  <BetaBadge />
  {#if canEdit}
    <div id="server-patching-actions" class="split-action relative" bind:this={menuRoot}>
      <button type="button" class="split-action-main" aria-haspopup="menu" onclick={() => (menuOpen = !menuOpen)}>Actions</button>
      <button type="button" class="split-action-caret" onclick={() => (menuOpen = !menuOpen)} aria-expanded={menuOpen} aria-haspopup="menu" aria-label="More actions">
        <span class="inline-flex transition-transform {menuOpen ? 'rotate-180' : ''}"><Reicon name="chevron-down" class="size-3" /></span>
      </button>
      {#if menuOpen}
        <div class="listbox-panel right-0! left-auto! xl:min-w-60! xl:max-w-96!" role="menu">
          <button type="button" class="listbox-option justify-start! gap-2.5!" role="menuitem" disabled={data?.checking} onclick={() => ((menuOpen = false), check())}>
            <Reicon name="refresh" class="size-3.5 opacity-70" />
            Check for updates
          </button>
          {#if removeUnusedSupported(packageManager)}
            <button type="button" class="listbox-option justify-start! gap-2.5!" role="menuitem" onclick={() => ((menuOpen = false), (removeUnusedOpen = true))}>
              <Reicon name="trash" class="size-3.5 opacity-70" />
              Remove unused packages
            </button>
          {/if}
          {#if rebootRequired}
            <button type="button" class="listbox-option justify-start! gap-2.5! text-red-500!" role="menuitem" onclick={() => ((menuOpen = false), (modal = 'reboot'))}>
              <Reicon name="restart" class="size-3.5 opacity-70" />
              Reboot server
            </button>
          {/if}
        </div>
      {/if}
    </div>
  {/if}
{/snippet}

{#snippet updatesBadges()}
  <StatusBadge label="{totalUpdates} {plural('update', totalUpdates)}" type={securityUpdates !== null && securityUpdates > 0 ? 'error' : 'warning'} />
{/snippet}

{#snippet updatesActions()}
  <div class="w-56">
    <div class="table-search relative min-w-0 w-full">
      <input
        type="search"
        placeholder="Search packages"
        aria-label="Search packages"
        maxlength={SEARCH_MAX_LENGTH}
        class="input w-full pl-8!"
        bind:value={search}
        oninput={searchChanged}
      />
      <div class="pointer-events-none absolute inset-y-0 left-0 flex items-center pl-2.5">
        <Reicon name="search" class="size-3.5 text-neutral-400 dark:text-fg-faint" />
      </div>
    </div>
  </div>
  {#if canEdit}
    {#if selectedPackages.length > 0}
      <button type="button" class="button" onclick={() => (selectedPackages = [])}>Clear selection</button>
      <button type="button" class="button flex gap-2" {...{ ishighlighted: '' }} onclick={() => (modal = 'selected')}>Update {selectedPackages.length} selected</button>
    {:else}
      <button type="button" class="button flex gap-2" {...{ ishighlighted: '' }} onclick={() => (modal = 'all')}>Update all packages</button>
    {/if}
  {/if}
{/snippet}

{#snippet heldBadges()}
  <StatusBadge label="{heldUpdates.length} held" type="warning" />
{/snippet}

{#snippet upToDateBadges()}
  <StatusBadge label="Up to date" type="success" />
{/snippet}

<ServerFrame {uuid} active="security" sub="patches" title="Patching" onloaded={load}>
  {#snippet children(f)}
    {#if dialog}
      <ServerActivityDialog {uuid} {activityId} title="Package Update Logs" onclose={() => ((dialog = false), loadResult())} />
    {/if}
    <div class="application-settings-form flex w-full min-w-0 flex-col gap-6">
      {#if !f.functional}
        <Empty size="sm" iconName="shield-alert" title="coolify-rs cannot reach {f.name}." description="Patching runs commands on the server. Validate the server first." />
      {:else if data}
        <form
          onsubmit={(e) => {
            e.preventDefault();
            save();
          }}
        >
          <UnsavedBar {dirty} {saving} onsave={save} onreset={() => (form = { ...saved })} />

          <SettingsSection id="server-patching-overview-section" title="Server patching" description="Discover and apply operating system package updates." {badges} {actions}>
            <div class="flex flex-col gap-4">
              <div class="flex flex-wrap items-center gap-2 text-sm text-neutral-600 dark:text-fg-dim">
                <span>
                  Last checked:
                  {#if data.checked_at}
                    <span title={checkedAtText}>{diffForHumans(data.checked_at)}</span>
                  {:else}
                    never
                  {/if}
                </span>
              </div>

              <div class="grid gap-4 {autoUpdatesSupported ? 'lg:grid-cols-3' : ''}">
                <FormInput
                  id="patchCheckFrequency"
                  label="Check frequency"
                  placeholder="weekly (default, on Sunday)"
                  helper="Cron expression or preset such as daily, weekly, or monthly. Leave empty for the default: weekly, at a fixed time on Sunday. coolify-rs notifies your team when updates are available and when a check starts to fail."
                  disabled={!canEdit}
                  bind:value={form.frequency}
                />
                {#if autoUpdatesSupported}
                  <div class="w-full">
                    <div class="mb-1.5 flex h-4 w-full items-center gap-1.5">
                      <!-- svelte-ignore a11y_label_has_associated_control -->
                      <label class="mb-0! flex items-center gap-1 text-sm font-medium leading-4">Automatic security updates</label>
                      <Helper
                        helper="{packageManager === 'apt' ? 'Uses unattended-upgrades for the security origins only.' : 'Uses dnf-automatic with upgrade_type = security.'} Turning it off removes the coolify-rs configuration and keeps the operating system default."
                      />
                    </div>
                    <div class="input flex items-center justify-between gap-2 pr-1!">
                      <span class="flex min-w-0 items-center gap-2 truncate text-neutral-700 dark:text-fg-dim">
                        <span class="size-1.5 shrink-0 rounded-full {data.auto_security_updates ? 'bg-emerald-500' : 'bg-neutral-400 dark:bg-neutral-500'}"></span>
                        {data.auto_security_updates ? 'On, every day' : 'Off'}
                      </span>
                      {#if canEdit}
                        <div class="flex shrink-0">
                          {#if data.auto_security_updates}
                            <button
                              type="button"
                              onclick={() => (modal = 'auto')}
                              class="h-6 shrink-0 rounded-md border border-neutral-200 bg-white px-2 text-[12px] font-medium text-neutral-800 hover:bg-neutral-50 dark:border-white/[0.1] dark:bg-white/[0.06] dark:text-fg dark:hover:bg-white/[0.1]"
                            >
                              Turn off
                            </button>
                          {:else}
                            <button type="button" {...{ ishighlighted: '' }} onclick={() => (modal = 'auto')} class="h-6 shrink-0 rounded-md border px-2 text-[12px] font-medium">Turn on</button>
                          {/if}
                        </div>
                      {/if}
                    </div>
                  </div>
                  <FormInput
                    id="autoSecurityUpdatesTime"
                    label="Maintenance window"
                    placeholder="03:00"
                    helper="Daily start time (24-hour HH:MM) in the time zone of the server. Updates that need a reboot are installed, but the server is not rebooted."
                    disabled={!canEdit}
                    bind:value={form.time}
                  />
                {/if}
              </div>

              <Callout type="info" title="Supported package managers">
                Automated package discovery currently supports apk, apt, dnf, pacman, and zypper. Automatic security updates are available for apt and dnf (Debian, Ubuntu, and RHEL family){#if packageManager && !autoUpdatesSupported}; this server uses {packageManager}{/if}. Status notifications can be managed from
                <a class="font-medium underline" href="/notifications/email" use:link>notification settings</a>.
              </Callout>
            </div>
          </SettingsSection>
        </form>

        {#if data.checking}
          <div>
            <SettingsSection title="Checking for updates" description="Package discovery runs in the background and can take several minutes.">
              <div class="flex items-center gap-3 py-4 text-sm text-neutral-600 dark:text-fg-dim">
                <div class="inline-flex items-center justify-center gap-2 text-[13px] text-neutral-500 dark:text-fg-dim" role="status" aria-live="polite">
                  <svg class="loading-indicator shrink-0 animate-spin size-4" viewBox="0 0 24 24" fill="none" aria-hidden="true">
                    <circle class="opacity-20" cx="12" cy="12" r="9" stroke="currentColor" stroke-width="2" />
                    <path class="opacity-80" d="M21 12a9 9 0 0 0-9-9" stroke="currentColor" stroke-width="2" stroke-linecap="round" />
                  </svg>
                </div>
                Inspecting installed packages…
              </div>
            </SettingsSection>
          </div>
        {:else if data.error}
          <SettingsSection id="server-package-updates-section" title="Package updates" description="Available operating system updates for this server.">
            <Callout type="danger" title="Could not check for updates">{data.error}</Callout>
          </SettingsSection>
        {:else if data.checked_at && totalUpdates === 0}
          <SettingsSection id="server-package-updates-section" title="Package updates" description="Available operating system updates for this server." badges={upToDateBadges}>
            <Empty size="sm" title="Server is up to date" description="No package updates are currently available." iconName="check-circle" />
          </SettingsSection>
        {:else if totalUpdates > 0}
          <SettingsSection
            id="server-package-updates-section"
            title="Package updates"
            description="{totalUpdates} {plural('update', totalUpdates)} available{securityUpdates !== null ? `, ${securityUpdates} of them security ${plural('update', securityUpdates)}` : ''}."
            flush
            badges={updatesBadges}
            actions={updatesActions}
          >
            {#if fullUpgradeOnly}
              <div class="px-4 pt-4">
                <Callout type="info" title="Full system upgrade only">Arch Linux does not support partial upgrades, so packages cannot be updated one by one.</Callout>
              </div>
            {/if}

            {#if shownUpdates.length === 0}
              <div class="p-6">
                <Empty size="sm" title="No matching updates" description="Try another package name, or clear the search." iconName="search" />
              </div>
            {:else}
              <div class="data-table">
                <div class="data-table-header package-updates-table-grid">
                  <span class="flex items-center">
                    {#if canSelect}
                      <input
                        type="checkbox"
                        checked={allMatchingSelected}
                        onclick={toggleAllPackages}
                        aria-label="Select all packages"
                        class="size-4 cursor-pointer rounded-sm border-neutral-300 text-accent focus:ring-accent dark:border-white/20 dark:bg-transparent"
                      />
                    {/if}
                  </span>
                  <span>Package</span>
                  <span>New version</span>
                </div>
                {#each shownUpdates as update (`${update.package}-${update.architecture}`)}
                  <div class="data-table-row package-updates-table-grid border-b border-neutral-200 last:border-b-0 dark:border-white/[0.08]">
                    <div class="flex items-center">
                      {#if canSelect}
                        <input
                          type="checkbox"
                          value={update.package}
                          checked={selectedPackages.includes(update.package)}
                          onchange={(e) => togglePackage(update.package, e.currentTarget.checked)}
                          id="select-package-{update.package}"
                          aria-label="Select {update.package}"
                          class="size-4 cursor-pointer rounded-sm border-neutral-300 text-accent focus:ring-accent dark:border-white/20 dark:bg-transparent"
                        />
                      {/if}
                    </div>
                    <label for="select-package-{update.package}" class="flex min-w-0 cursor-pointer items-center gap-2">
                      {#if update.risk}
                        <span class="shrink-0" data-tooltip={RISK_LABELS[update.risk] ?? ''}>
                          <Reicon name="alert-triangle" class="size-4 text-red-500" />
                          <span class="sr-only">{RISK_LABELS[update.risk] ?? ''}</span>
                        </span>
                      {/if}
                      <span class="min-w-0 truncate font-mono text-[12px] text-neutral-950 dark:text-fg">{update.package}</span>
                      {#if update.security}
                        <StatusBadge label="Security" type="error" />
                      {/if}
                    </label>
                    <div class="min-w-0">
                      <p class="truncate font-mono text-[12px] text-neutral-700 dark:text-fg-dim">{update.new_version}</p>
                      {#if update.current_version && update.current_version !== 'unknown'}
                        <p class="mt-0.5 truncate text-[10px] text-neutral-500 dark:text-fg-faint">Current: {update.current_version}</p>
                      {/if}
                    </div>
                  </div>
                {/each}
                <Pager total={matchingUpdates.length} bind:page bind:pageSize={perPage} storageKey="coolify.page-size.server-package-updates" separator="–" />
              </div>
            {/if}
          </SettingsSection>
        {/if}

        {#if !data.checking && !data.error && heldUpdates.length > 0}
          <SettingsSection
            id="server-held-updates-section"
            title="Held back updates"
            description="The package manager does not install these updates. Phased rollout: Ubuntu releases the update to a part of all servers at a time, and this server gets it in a later phase. Held: a package hold (apt-mark hold) or a version pin (/etc/apk/world) on the server blocks the update."
            flush
            badges={heldBadges}
          >
            <div class="data-table">
              <div class="data-table-header held-updates-table-grid">
                <span>Package</span>
                <span>New version</span>
                <span class="text-right">Reason</span>
              </div>
              {#each heldUpdates as update (`${update.package}-${update.architecture}`)}
                <div class="data-table-row held-updates-table-grid border-b border-neutral-200 last:border-b-0 dark:border-white/[0.08]">
                  <span class="min-w-0 truncate font-mono text-[12px] text-neutral-950 dark:text-fg">{update.package}</span>
                  <div class="min-w-0">
                    <p class="truncate font-mono text-[12px] text-neutral-700 dark:text-fg-dim">{update.new_version}</p>
                    <p class="mt-0.5 truncate text-[10px] text-neutral-500 dark:text-fg-faint">Current: {update.current_version}</p>
                  </div>
                  <div class="flex justify-end">
                    <StatusBadge label={HELD_REASON_LABELS[update.held_reason ?? ''] ?? 'Held'} type="warning" />
                  </div>
                </div>
              {/each}
            </div>
          </SettingsSection>
        {/if}
      {/if}
    </div>

    {#if data}
      <!-- The dialog of the "Remove unused packages" menu item (upstream: a hidden trigger). -->
      {#if canEdit}
        <RemoveUnusedPackagesButton {packageManager} trigger={false} bind:open={removeUnusedOpen} onconfirm={() => start(`${url}/remove-unused`)} />
      {/if}
      {#if modal === 'reboot'}
        <ConfirmModal
          title="Reboot server?"
          warningMessage="The containers on this server stop until the server is back."
          actions={[
            'The server reboots in a few seconds.',
            'All containers on this server stop, and the containers with a restart policy start again after the reboot.',
            'The server is not reachable for a few minutes.',
          ]}
          confirmationText={data.name}
          confirmationLabel="Enter the server name to confirm"
          shortConfirmationLabel="Server name"
          confirmWithText={data.confirm_with_text}
          step2ButtonText="Reboot server"
          onconfirm={reboot}
          onclose={() => (modal = null)}
        />
      {:else if modal === 'auto'}
        {#if data.auto_security_updates}
          <ConfirmModal
            title="Turn off automatic security updates?"
            warningMessage="Security fixes are then not installed automatically by coolify-rs."
            confirmWithText={false}
            actions={['coolify-rs removes its configuration from the server.', 'The server uses the default of the operating system again. Check it if you want no automatic updates at all.']}
            step2ButtonText="Turn off"
            onconfirm={toggleAuto}
            onclose={() => (modal = null)}
          />
        {:else}
          <ConfirmModal
            title="Turn on automatic security updates?"
            warningMessage="Updates run without you. Your resources can stop for a short time during an update. You can turn this off at any time."
            confirmWithText={false}
            actions={[
              'Every day at the time in Maintenance window (time zone of the server), the server installs security updates without asking.',
              'If Docker comes from the operating system packages (for example docker.io or moby-engine), a security update of Docker restarts it, and all containers on this server stop and start again. Docker from the Docker repository (docker-ce, as coolify-rs installs it) has no security updates there and is not updated.',
              'Services that use an updated library (for example OpenSSL or glibc) can restart.',
              'A new kernel is installed, but the server is not rebooted. coolify-rs then shows Reboot required.',
            ]}
            step2ButtonText="Turn on"
            onconfirm={toggleAuto}
            onclose={() => (modal = null)}
          />
        {/if}
      {:else if modal === 'all'}
        <ConfirmModal
          title="Confirm package update?"
          actions={[
            'All packages will be updated to their latest available versions.',
            riskyUpdates > 0 ? `${riskyUpdates} ${plural('update', riskyUpdates)} can restart Docker, the running containers, or core system services, or need a reboot.` : 'Some services on the server can restart.',
          ]}
          confirmationText="Update All Packages"
          confirmationLabel="Confirm by entering the text below"
          shortConfirmationLabel="Confirmation"
          confirmWithText={data.confirm_with_text}
          step2ButtonText="Update All Packages"
          onconfirm={async () => {
            await start(`${url}/update-all`);
          }}
          onclose={() => (modal = null)}
        />
      {:else if modal === 'selected'}
        <ConfirmModal
          title="Update selected packages?"
          actions={[
            `coolify-rs updates ${selectedPackages.join(', ')}.`,
            selectedRisky > 0 ? `${selectedRisky} of them can restart Docker, the running containers, or core system services, or need a reboot.` : 'Some services on the server can restart.',
          ]}
          warningMessage="coolify-rs cannot roll back a package update. Check the update logs when it is done."
          confirmWithText={false}
          confirmWithPassword={false}
          step2ButtonText="Update"
          onconfirm={updateSelected}
          onclose={() => (modal = null)}
        />
      {/if}
    {/if}
  {/snippet}
</ServerFrame>
