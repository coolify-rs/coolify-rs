<script lang="ts">
  // Port of upstream livewire/server/security/patches.blade.php.
  import { onDestroy } from 'svelte';
  import BetaBadge from '../components/BetaBadge.svelte';
  import Callout from '../components/Callout.svelte';
  import ConfirmModal from '../components/ConfirmModal.svelte';
  import Empty from '../components/Empty.svelte';
  import FormInput from '../components/FormInput.svelte';
  import Helper from '../components/Helper.svelte';
  import Reicon from '../components/Reicon.svelte';
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
  };

  // CheckUpdates::RISK_LABELS, HELD_REASON_LABELS, FULL_UPGRADE_ONLY
  const RISK_LABELS: Record<string, string> = {
    docker: 'Restarts Docker and the running containers',
    kernel: 'Needs a reboot to apply',
    system: 'Restarts core system services',
  };
  const HELD_REASON_LABELS: Record<string, string> = { phased: 'Phased rollout', held: 'Held' };

  let data = $state<Data | null>(null);
  let form = $state({ frequency: '', time: '03:00' });
  let saved = $state({ frequency: '', time: '03:00' });
  let saving = $state(false);
  let modal = $state<'reboot' | 'auto' | 'all' | null>(null);
  let dialog = $state(false);
  let activityId = $state<number | null>(null);
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

  /** `loadResult`: everything but the form. */
  async function loadResult() {
    const d = await get<Data>(url).catch(() => null);
    if (d && data) {
      data.result = d.result;
      data.error = d.error;
      data.checked_at = d.checked_at;
      data.checking = d.checking;
      data.auto_security_updates = d.auto_security_updates;
    }
  }
  /** `syncSettings` and `loadResult`. */
  async function load() {
    const d = await get<Data>(url);
    data = d;
    form = { frequency: d.patch_check_frequency, time: d.auto_security_updates_time };
    saved = { ...form };
    clearInterval(poll);
    // `wire:poll.10s` while checking, and the `ServerPatchCheckFinished` broadcast.
    poll = setInterval(loadResult, 10000);
  }
  onDestroy(() => clearInterval(poll));

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
  async function start(path: string, body: unknown = {}) {
    try {
      const r = await post<{ activity_id: number }>(path, body);
      activityId = r.activity_id;
      dialog = true;
    } catch (e) {
      failure(e);
    }
  }
  async function toggleAuto() {
    await action(() => post(`${url}/auto-updates`, { autoSecurityUpdatesTime: form.time }));
    await load();
  }
  async function reboot() {
    await action(() => post(`${url}/reboot`));
  }
</script>

{#snippet actions()}
  <BetaBadge />
  {#if canEdit}
    {#if rebootRequired}
      <button type="button" class="button button-error" onclick={() => (modal = 'reboot')}>Reboot server</button>
    {/if}
    <button type="button" class="button" disabled={data?.checking} onclick={check}>
      <Reicon name="refresh" class="size-3.5" />
      Check for updates
    </button>
  {/if}
{/snippet}

{#snippet allAction()}
  {#if canEdit}
    <button type="button" class="button flex gap-2" {...{ ishighlighted: '' }} onclick={() => (modal = 'all')}>Update all packages</button>
  {/if}
{/snippet}

<ServerFrame {uuid} active="security" sub="patches" title="Security" onloaded={load}>
  {#snippet children()}
    {#if dialog}
      <ServerActivityDialog {uuid} {activityId} title="Package Update Logs" onclose={() => ((dialog = false), loadResult())} />
    {/if}
    {#if data}
      <div class="application-settings-form flex w-full flex-col gap-6">
        <form
          onsubmit={(e) => {
            e.preventDefault();
            save();
          }}
        >
          <UnsavedBar {dirty} {saving} onsave={save} onreset={() => (form = { ...saved })} />

          <SettingsSection id="server-patching-overview-section" title="Server patching" description="Discover and apply operating system package updates." {actions}>
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
                {#if data.checked_at && !data.error}
                  {#if totalUpdates === 0}
                    <StatusBadge label="Up to date" type="success" />
                  {/if}
                  {#if securityUpdates !== null && securityUpdates > 0}
                    <StatusBadge label="{securityUpdates} {plural('security update', securityUpdates)}" type="error" />
                  {/if}
                  {#if rebootRequired}
                    <StatusBadge label="Reboot required" type="warning" />
                  {/if}
                {/if}
              </div>

              <div class="grid gap-4 {autoUpdatesSupported ? 'lg:grid-cols-3' : ''}">
                <FormInput
                  id="patchCheckFrequency"
                  label="Check frequency"
                  placeholder="weekly (default, on Sunday)"
                  helper="Cron expression or preset such as daily, weekly, or monthly. Leave empty for the default: weekly, at a fixed time on Sunday. Coolify notifies your team when updates are available and when a check starts to fail."
                  disabled={!canEdit}
                  bind:value={form.frequency}
                />
                {#if autoUpdatesSupported}
                  <div class="w-full">
                    <div class="mb-1.5 flex h-4 w-full items-center gap-1.5">
                      <!-- svelte-ignore a11y_label_has_associated_control -->
                      <label class="mb-0! flex items-center gap-1 text-sm font-medium leading-4">Automatic security updates</label>
                      <Helper
                        helper="{packageManager === 'apt' ? 'Uses unattended-upgrades for the security origins only.' : 'Uses dnf-automatic with upgrade_type = security.'} Turning it off removes the Coolify configuration and keeps the operating system default."
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
          <SettingsSection title="Package updates" description="Available operating system updates for this server.">
            <Callout type="danger" title="Could not check for updates">{data.error}</Callout>
          </SettingsSection>
        {:else if data.checked_at && totalUpdates === 0}
          <SettingsSection title="Package updates" description="Available operating system updates for this server.">
            <Empty size="sm" title="Server is up to date" description="No package updates are currently available." iconName="check-circle" />
          </SettingsSection>
        {:else if totalUpdates > 0}
          <SettingsSection
            id="server-package-updates-section"
            title="Package updates"
            description="{totalUpdates} {plural('update', totalUpdates)} available{securityUpdates !== null ? `, ${securityUpdates} of them security ${plural('update', securityUpdates)}` : ''}."
            flush
            actions={allAction}
          >
            {#if fullUpgradeOnly}
              <div class="px-4 pt-4">
                <Callout type="info" title="Full system upgrade only">Arch Linux does not support partial upgrades, so packages cannot be updated one by one.</Callout>
              </div>
            {/if}

            <div class="data-table">
              <div class="data-table-header package-updates-table-grid">
                <span>Package</span>
                <span>New version</span>
                <span class="text-right">Action</span>
              </div>
              {#each updates as update (`${update.package}-${update.architecture}`)}
                <div class="data-table-row package-updates-table-grid border-b border-neutral-200 last:border-b-0 dark:border-white/[0.08]">
                  <div class="flex min-w-0 items-center gap-2">
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
                  </div>
                  <div class="min-w-0">
                    <p class="truncate font-mono text-[12px] text-neutral-700 dark:text-fg-dim">{update.new_version}</p>
                    {#if update.current_version && update.current_version !== 'unknown'}
                      <p class="mt-0.5 truncate text-[10px] text-neutral-500 dark:text-fg-faint">Current: {update.current_version}</p>
                    {/if}
                  </div>
                  <div class="flex justify-end">
                    {#if !fullUpgradeOnly && canEdit}
                      <button type="button" class="button" onclick={() => start(`${url}/update`, { package: update.package })}>Update</button>
                    {/if}
                  </div>
                </div>
              {/each}
              <div class="flex min-h-11 items-center border-t border-neutral-200 px-4 text-[11px] text-neutral-500 dark:border-white/[0.08] dark:text-fg-faint">
                {totalUpdates} {plural('package update', totalUpdates)}
              </div>
            </div>
          </SettingsSection>
        {/if}

        {#if !data.checking && !data.error && heldUpdates.length > 0}
          <SettingsSection
            id="server-held-updates-section"
            title="Held back updates"
            description="The package manager does not install these updates. Phased rollout: Ubuntu releases the update to a part of all servers at a time, and this server gets it in a later phase. Held: a package hold (apt-mark hold) or a version pin (/etc/apk/world) on the server blocks the update."
            flush
          >
            <div class="data-table">
              <div class="data-table-header package-updates-table-grid">
                <span>Package</span>
                <span>New version</span>
                <span class="text-right">Reason</span>
              </div>
              {#each heldUpdates as update (`${update.package}-${update.architecture}`)}
                <div class="data-table-row package-updates-table-grid border-b border-neutral-200 last:border-b-0 dark:border-white/[0.08]">
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
      </div>

      {#if modal === 'reboot'}
        <ConfirmModal
          title="Reboot server?"
          actions={[
            'The server reboots in a few seconds.',
            'All containers on this server stop, and the containers with a restart policy start again after the reboot.',
            'The server is not reachable for a few minutes.',
          ]}
          confirmationText={data.name}
          confirmationLabel="Enter the server name to confirm"
          shortConfirmationLabel="Server name"
          step2ButtonText="Reboot server"
          onconfirm={reboot}
          onclose={() => (modal = null)}
        />
      {:else if modal === 'auto'}
        {#if data.auto_security_updates}
          <ConfirmModal
            title="Turn off automatic security updates?"
            warningMessage="Security fixes are then not installed automatically by Coolify."
            confirmWithText={false}
            actions={['Coolify removes its configuration from the server.', 'The server uses the default of the operating system again. Check it if you want no automatic updates at all.']}
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
              'If Docker comes from the operating system packages (for example docker.io or moby-engine), a security update of Docker restarts it, and all containers on this server stop and start again. Docker from the Docker repository (docker-ce, as Coolify installs it) has no security updates there and is not updated.',
              'Services that use an updated library (for example OpenSSL or glibc) can restart.',
              'A new kernel is installed, but the server is not rebooted. Coolify then shows Reboot required.',
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
          step2ButtonText="Update All Packages"
          onconfirm={() => start(`${url}/update-all`)}
          onclose={() => (modal = null)}
        />
      {/if}
    {/if}
  {/snippet}
</ServerFrame>
