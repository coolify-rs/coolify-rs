<script lang="ts">
  // coolify-rs's own (ADR 0010, ADR 0013): Settings → Plugins, for instance admins. Coolify has no
  // plugins; the page is built from the settings pages' parts (sections, checkboxes, the confirmation
  // modal) and is only reachable when the menu shows it.
  import Callout from '../components/Callout.svelte';
  import CapabilityList from '../components/CapabilityList.svelte';
  import Checkbox from '../components/Checkbox.svelte';
  import ConfirmModal from '../components/ConfirmModal.svelte';
  import Empty from '../components/Empty.svelte';
  import Modal from '../components/Modal.svelte';
  import PluginPanels from '../components/PluginPanels.svelte';
  import Reicon from '../components/Reicon.svelte';
  import SettingsLayout from '../components/SettingsLayout.svelte';
  import SettingsSection from '../components/SettingsSection.svelte';
  import { ApiError } from '../lib/api';
  import { router } from '../lib/router.svelte';
  import { notify } from '../lib/toast.svelte';
  import {
    pluginNav,
    pluginsUi,
    readFile,
    type Approval,
    type CatalogEntry,
    type InstalledPlugin,
    type PendingPlugin,
    type SettingsData,
  } from '../lib/plugins.svelte';

  document.title = 'Plugins | coolify-rs';

  let data = $state<SettingsData | null>(null);
  /** `null` while loading or when this instance has no catalogue (the section is hidden). */
  let catalog = $state<CatalogEntry[] | null>(null);
  let removing = $state<InstalledPlugin | null>(null);
  let busy = $state<string | null>(null);

  let manifestFile = $state<File | null>(null);
  let componentFile = $state<File | null>(null);
  let manifestInput: HTMLInputElement | undefined = $state();
  let componentInput: HTMLInputElement | undefined = $state();

  /** The capabilities waiting for approval, and what approving does. */
  let approval = $state<{ title: string; plugin?: PendingPlugin; capabilities: string[]; unverified: boolean; done: string; confirm: (grant: string[]) => Promise<Approval> } | null>(null);
  let approving = $state(false);

  const leave = () => router.go('/', true);
  const message = (e: unknown) => (e instanceof ApiError ? e.message : String(e));

  async function load() {
    let d: SettingsData;
    try {
      d = await pluginsUi.settings();
    } catch (e) {
      // Plugins switched off, or not an instance admin: the page does not exist (`Route::any`).
      if (e instanceof ApiError && (e.status === 404 || e.status === 403)) return leave();
      throw e;
    }
    data = d;
  }

  async function loadCatalog() {
    try {
      catalog = await pluginsUi.catalog();
    } catch {
      catalog = null;
    }
  }

  async function refreshAll() {
    await Promise.all([load(), loadCatalog(), pluginNav.load()]);
  }

  Promise.all([load(), loadCatalog()]).then(() => {
    // Nothing installed, nothing to install from: the menu has no entry and the page does not exist.
    if (data && !data.plugins.length && !data.allow_upload && catalog === null) leave();
  });

  async function setEnabled(p: InstalledPlugin, enabled: boolean) {
    busy = p.id;
    try {
      await pluginsUi.setEnabled(p.id, enabled);
      notify.success(enabled ? 'Plugin enabled.' : 'Plugin disabled.');
    } catch (e) {
      notify.failure(message(e));
    } finally {
      busy = null;
      await refreshAll();
    }
  }

  async function uninstall(password: string): Promise<string | void> {
    if (!removing) return;
    try {
      const r = await pluginsUi.uninstall(removing.id, password);
      notify.success(r.message);
    } catch (e) {
      if (e instanceof ApiError && e.field('password')) return e.field('password');
      // It still owns resources in Coolify's tables (ADR 0012): say so and close the modal.
      if (e instanceof ApiError && e.status === 409) {
        notify.failure(e.message);
        return;
      }
      throw e;
    }
    await refreshAll();
  }

  /** The first request answers what the plugin asks for; the admin approves exactly that list. */
  async function ask(title: string, unverified: boolean, run: (grant?: string[]) => Promise<Approval>, done = 'Plugin installed.') {
    try {
      const first = await run();
      if (first.done) {
        notify.success(done);
        await refreshAll();
        return;
      }
      approval = { title, unverified, done, plugin: first.plugin, capabilities: first.capabilities, confirm: (grant) => run(grant) };
    } catch (e) {
      notify.failure(message(e));
    }
  }

  async function approve() {
    if (!approval) return;
    approving = true;
    try {
      const r = await approval.confirm($state.snapshot(approval.capabilities));
      if (!r.done) {
        // The plugin changed between the two requests: show the new list.
        approval = { ...approval, capabilities: r.capabilities, plugin: r.plugin ?? approval.plugin };
        notify.failure('The capabilities changed. Review them again.');
        return;
      }
      notify.success(approval.done);
      approval = null;
      manifestFile = null;
      componentFile = null;
      await refreshAll();
    } catch (e) {
      notify.failure(message(e));
    } finally {
      approving = false;
    }
  }

  async function uploadFiles() {
    if (!manifestFile || !componentFile) return;
    busy = 'upload';
    try {
      const [manifest, component] = await Promise.all([readFile(manifestFile, 'text'), readFile(componentFile, 'base64')]);
      await ask('Install plugin', true, (grant) => pluginsUi.upload({ manifest, component, grant }));
    } catch (e) {
      notify.failure(message(e));
    } finally {
      busy = null;
    }
  }

  async function refreshCatalog() {
    busy = 'catalog';
    try {
      await pluginsUi.refreshCatalog();
      notify.success('Plugin registry refreshed.');
    } catch (e) {
      notify.failure(message(e));
    } finally {
      busy = null;
      await loadCatalog();
    }
  }
</script>

{#snippet badges(p: { version: string; verified?: boolean; enabled?: boolean; disabled_reason?: string | null })}
  <span class="table-badge">v{p.version}</span>
  {#if p.verified === false}
    <span class="table-badge table-badge-warning" title="Installed outside the plugin registry">Unverified</span>
  {/if}
  {#if p.enabled === false}
    <span class="table-badge table-badge-danger">{p.disabled_reason ? 'Switched off' : 'Disabled'}</span>
  {/if}
{/snippet}

{#snippet fact(label: string, value: string)}
  <div class="min-w-0">
    <dt class="text-[11px] text-neutral-500 dark:text-fg-faint">{label}</dt>
    <dd class="truncate text-[12px] text-neutral-700 dark:text-fg-dim" title={value}>{value}</dd>
  </div>
{/snippet}

{#snippet filePicker(id: string, label: string, accept: string, file: File | null, pick: () => void)}
  <div class="flex min-w-0 flex-col gap-1.5">
    <span id="{id}-label" class="text-[12px] font-medium text-neutral-700 dark:text-fg-dim">{label}</span>
    <div class="flex min-w-0 items-center gap-2">
      <button type="button" class="button" aria-labelledby="{id}-label" onclick={pick}>Choose file</button>
      <span class="min-w-0 truncate text-[12px] text-neutral-500 dark:text-fg-faint">{file ? file.name : `No ${accept} file chosen`}</span>
    </div>
  </div>
{/snippet}

<div>
  <SettingsLayout>
    {#if data}
      <div class="application-settings-form flex min-w-0 flex-col gap-6">
        <SettingsSection id="installed-plugins-section" title="Installed plugins" description="Plugins run in a sandbox and can only do what you approved when installing them. Each team switches them on for itself.">
          {#if !data.plugins.length}
            <Empty title="No plugins installed" description="Install a plugin from the registry{data.allow_upload ? ' or from files' : ''}." iconName="browser-code" size="sm" />
          {:else}
            <div class="flex flex-col gap-4">
              {#each data.plugins as p (p.id)}
                <div class="rounded-lg border border-neutral-200 p-4 dark:border-white/[0.08]" data-plugin={p.id}>
                  <div class="flex flex-wrap items-start justify-between gap-3">
                    <div class="min-w-0">
                      <div class="flex flex-wrap items-center gap-2">
                        <h4 class="text-[13px] font-semibold text-black dark:text-fg">{p.name}</h4>
                        {@render badges(p)}
                      </div>
                      <p class="mt-0.5 text-[12px] text-neutral-500 dark:text-fg-dim">{p.description}</p>
                    </div>
                    <div class="flex shrink-0 items-center gap-2">
                      <Checkbox id="plugin-enabled-{p.id}" label="Enabled" helper="Switches the plugin on or off for every team." checked={p.enabled} disabled={busy === p.id} onchange={(v) => setEnabled(p, v)} />
                    </div>
                  </div>
                  {#if p.disabled_reason}
                    <Callout type="danger" title="Switched off automatically" class="mt-3">{p.disabled_reason}</Callout>
                  {/if}
                  <dl class="mt-3 grid gap-3 sm:grid-cols-2 lg:grid-cols-4">
                    {@render fact('ID', p.id)}
                    {@render fact('Origin', p.verified ? 'Registry' : p.origin === 'upload' ? 'Uploaded file' : p.origin)}
                    {@render fact('License', p.license)}
                    {@render fact('SHA-256', p.sha256)}
                  </dl>
                  {#if p.source}
                    <p class="mt-2 text-[12px]">
                      <a href={p.source} target="_blank" rel="noopener noreferrer" class="inline-flex items-center gap-1 underline underline-offset-2">
                        Source <Reicon name="external-link" class="size-3" />
                      </a>
                    </p>
                  {/if}
                  <div class="mt-3">
                    <h5 class="mb-1.5 text-[12px] font-medium text-neutral-700 dark:text-fg-dim">Granted capabilities</h5>
                    <CapabilityList capabilities={p.capabilities} />
                  </div>
                  <div class="mt-4 flex justify-end border-t border-neutral-200 pt-3 dark:border-white/[0.08]">
                    <button type="button" class="button button-error w-auto" onclick={() => (removing = p)}>Uninstall</button>
                  </div>
                </div>
              {/each}
            </div>
          {/if}
        </SettingsSection>

        {#if catalog !== null}
          <SettingsSection id="plugin-registry-section" title="Plugin registry" description="Plugins reviewed and signed by the coolify-rs plugin registry.">
            {#snippet actions()}
              <button type="button" class="button" disabled={busy === 'catalog'} onclick={refreshCatalog}>Refresh</button>
            {/snippet}
            {#if !catalog.length}
              <Empty title="The registry is empty" iconName="browser-code" size="sm" />
            {:else}
              <div class="flex flex-col gap-4">
                {#each catalog as c (c.id)}
                  <div class="rounded-lg border border-neutral-200 p-4 dark:border-white/[0.08]" data-catalog={c.id}>
                    <div class="flex flex-wrap items-start justify-between gap-3">
                      <div class="min-w-0">
                        <div class="flex flex-wrap items-center gap-2">
                          <h4 class="text-[13px] font-semibold text-black dark:text-fg">{c.name}</h4>
                          <span class="table-badge">v{c.latest_version}</span>
                          {#if c.installed_version}<span class="table-badge table-badge-success">Installed v{c.installed_version}</span>{/if}
                          {#if c.update_available}<span class="table-badge table-badge-warning">Update available</span>{/if}
                          {#if c.yanked}<span class="table-badge table-badge-warning">Yanked</span>{/if}
                          {#if c.revoked}<span class="table-badge table-badge-danger">Revoked</span>{/if}
                        </div>
                        <p class="mt-0.5 text-[12px] text-neutral-500 dark:text-fg-dim">{c.description}</p>
                        <p class="mt-0.5 text-[11px] text-neutral-500 dark:text-fg-faint">{c.id} · {c.license}</p>
                      </div>
                      <div class="flex shrink-0 items-center gap-2">
                        {#if c.revoked}
                          <!-- A revoked plugin cannot be installed or updated. -->
                        {:else if !c.installed_version && !c.yanked}
                          <button type="button" class="button button-highlighted" onclick={() => ask(`Install ${c.name}`, false, (grant) => pluginsUi.install(c.id, grant))}>Install</button>
                        {:else if c.pending_update}
                          <button type="button" class="button button-highlighted" onclick={() => ask(`Update ${c.name} to v${c.pending_update?.version}`, false, (grant) => pluginsUi.update(c.id, grant, c.pending_update?.version), 'Plugin updated.')}>Review update</button>
                        {:else if c.update_available}
                          <button type="button" class="button button-highlighted" onclick={() => ask(`Update ${c.name}`, false, (grant) => pluginsUi.update(c.id, grant), 'Plugin updated.')}>Update</button>
                        {/if}
                      </div>
                    </div>
                    {#if c.revoked}
                      <Callout type="danger" title="Revoked" class="mt-3">{c.revoked.reason}</Callout>
                    {/if}
                    {#if c.pending_update}
                      <Callout type="warning" title="Update waiting for approval" class="mt-3">
                        Version {c.pending_update.version} asks for more than the installed version was granted, so it is not installed until you approve its capabilities.
                      </Callout>
                    {/if}
                    <div class="mt-3">
                      <h5 class="mb-1.5 text-[12px] font-medium text-neutral-700 dark:text-fg-dim">Capabilities</h5>
                      <CapabilityList capabilities={c.capabilities} />
                    </div>
                  </div>
                {/each}
              </div>
            {/if}
          </SettingsSection>
        {/if}

        {#if data.allow_upload}
          <SettingsSection id="plugin-upload-section" title="Install from files" description="Development mode: install a plugin you built yourself. It is pinned by its hash and shown as Unverified.">
            <Callout type="warning" title="Unverified plugins" class="mb-4">
              Plugins installed from files have not been reviewed by the plugin registry. Install only plugins whose source you trust.
            </Callout>
            <div class="grid gap-4 lg:grid-cols-2">
              {@render filePicker('plugin-manifest', 'Manifest (plugin.toml)', '.toml', manifestFile, () => manifestInput?.click())}
              {@render filePicker('plugin-component', 'Component (.wasm)', '.wasm', componentFile, () => componentInput?.click())}
            </div>
            <input bind:this={manifestInput} type="file" accept=".toml" class="hidden" onchange={(e) => (manifestFile = e.currentTarget.files?.[0] ?? null)} />
            <input bind:this={componentInput} type="file" accept=".wasm,application/wasm" class="hidden" onchange={(e) => (componentFile = e.currentTarget.files?.[0] ?? null)} />
            <div class="mt-4 flex justify-end">
              <button type="button" class="button button-highlighted" disabled={!manifestFile || !componentFile || busy === 'upload'} onclick={uploadFiles}>Continue</button>
            </div>
          </SettingsSection>
        {/if}

        <PluginPanels slot="settings" />
      </div>
    {/if}
  </SettingsLayout>
</div>

{#if approval}
  <Modal title={approval.title} subtitle={approval.plugin ? `${approval.plugin.name} v${approval.plugin.version} (${approval.plugin.id})` : undefined} onclose={() => (approval = null)} narrow>
    {#if approval.unverified}
      <Callout type="warning" title="Unverified" class="mb-4">This plugin is not from the plugin registry. Nobody has reviewed what it does.</Callout>
    {/if}
    {#if approval.plugin?.description}
      <p class="mb-3 text-[12px] text-neutral-600 dark:text-fg-dim">{approval.plugin.description}</p>
    {/if}
    <h4 class="mb-2 text-[12px] font-semibold">The plugin will be granted exactly these capabilities:</h4>
    <CapabilityList capabilities={approval.capabilities} />
    <p class="mt-3 text-[12px] text-neutral-500 dark:text-fg-dim">It runs for a team only after that team switches it on.</p>
    {#snippet footer()}
      <button type="button" class="button" onclick={() => (approval = null)}>Cancel</button>
      <button type="button" class="button button-highlighted" disabled={approving} onclick={approve}>Approve and install</button>
    {/snippet}
  </Modal>
{/if}

{#if removing}
  <ConfirmModal
    title="Uninstall {removing.name}?"
    actions={['The plugin is removed for every team.', 'Its stored data is deleted for every team.']}
    confirmWithText={data?.confirm.text ?? true}
    confirmWithPassword={data?.confirm.password ?? true}
    confirmationText={removing.id}
    confirmationLabel="Please confirm by entering the plugin ID below"
    shortConfirmationLabel="Plugin ID"
    onconfirm={uninstall}
    onclose={() => (removing = null)}
  />
{/if}
