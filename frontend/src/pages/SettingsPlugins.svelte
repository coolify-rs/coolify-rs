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
    type Catalog,
    type CatalogEntry,
    type InstalledPlugin,
    type PendingPlugin,
    type SettingsData,
  } from '../lib/plugins.svelte';

  document.title = 'Plugins | coolify-rs';

  let data = $state<SettingsData | null>(null);
  /** The registry catalogue; `null` while loading. `catalogError` when it could not be read. */
  let catalog = $state<Catalog | null>(null);
  let catalogError = $state<string | null>(null);
  let removing = $state<InstalledPlugin | null>(null);
  let busy = $state<string | null>(null);

  let manifestFile = $state<File | null>(null);
  let componentFile = $state<File | null>(null);
  let manifestInput: HTMLInputElement | undefined = $state();
  let componentInput: HTMLInputElement | undefined = $state();

  /** Install or update: `grant` approves exactly the list shown, `version` pins what was shown. */
  type Run = (grant?: string[], version?: string) => Promise<Approval>;
  /** The capabilities waiting for approval, and what approving does. */
  let approval = $state<{
    title: string;
    plugin?: PendingPlugin;
    capabilities: string[];
    added?: string[];
    version?: string;
    unverified: boolean;
    done: string;
    run: Run;
  } | null>(null);
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
      catalogError = null;
    } catch (e) {
      catalogError = message(e);
    }
  }

  async function refreshAll() {
    await Promise.all([load(), loadCatalog(), pluginNav.load()]);
  }

  load();
  loadCatalog();

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
  async function ask(title: string, unverified: boolean, run: Run, done = 'Plugin installed.', version?: string) {
    try {
      const first = await run(undefined, version);
      if (first.done) {
        notify.success(done);
        await refreshAll();
        return;
      }
      approval = {
        title,
        unverified,
        done,
        run,
        plugin: first.plugin,
        capabilities: first.capabilities,
        added: first.added,
        version: first.version ?? version,
      };
    } catch (e) {
      notify.failure(message(e));
      await loadCatalog();
    }
  }

  async function approve() {
    if (!approval) return;
    approving = true;
    try {
      const r = await approval.run($state.snapshot(approval.capabilities), approval.version);
      if (!r.done) {
        // The plugin changed between the two requests: show the new list.
        approval = { ...approval, capabilities: r.capabilities, added: r.added, version: r.version ?? approval.version, plugin: r.plugin ?? approval.plugin };
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
      const r = await pluginsUi.refreshCatalog();
      const notes = [
        `${r.plugins} ${r.plugins === 1 ? 'plugin' : 'plugins'} in the registry.`,
        r.revoked.length ? `Switched off as revoked: ${r.revoked.join(', ')}.` : '',
        r.pending_updates.length ? `Updates waiting for approval: ${r.pending_updates.join(', ')}.` : '',
      ];
      notify.success(`Plugin registry refreshed. ${notes.filter(Boolean).join(' ')}`);
    } catch (e) {
      notify.failure(message(e));
    } finally {
      busy = null;
      await refreshAll();
    }
  }

  const date = (s: string) => new Date(s).toLocaleString();
  /** What the catalogue lets the admin do with a plugin. */
  function action(c: CatalogEntry): { label: string; title: string; done: string; version?: string; update: boolean } | null {
    if (c.revoked) return null;
    if (c.pending_update) return { label: 'Review update', title: `Update ${c.name} to v${c.pending_update.version}`, done: 'Plugin updated.', version: c.pending_update.version, update: true };
    if (c.installed_version && c.update_available && c.latest_version)
      return { label: 'Update', title: `Update ${c.name} to v${c.latest_version}`, done: 'Plugin updated.', version: c.latest_version, update: true };
    if (!c.installed_version && c.latest_version) return { label: 'Install', title: `Install ${c.name}`, done: 'Plugin installed.', version: c.latest_version, update: false };
    return null;
  }
  function run(c: CatalogEntry) {
    const a = action(c);
    if (!a) return;
    const call: Run = a.update ? (grant, version) => pluginsUi.update(c.id, grant, version) : (grant, version) => pluginsUi.install(c.id, grant, version);
    ask(a.title, false, call, a.done, a.version);
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

        <SettingsSection id="plugin-registry-section" title="Plugin registry" description="Plugins reviewed and signed by the coolify-rs plugin registry. The index is refreshed once a day.">
          {#snippet actions()}
            <button type="button" class="button" disabled={busy === 'catalog'} onclick={refreshCatalog}>Refresh</button>
          {/snippet}
          {#if catalogError}
            <Callout type="danger" title="The catalogue could not be read" class="mb-4">{catalogError}</Callout>
          {/if}
          {#if catalog}
            {#if catalog.index}
              <p class="mb-4 text-[11px] text-neutral-500 dark:text-fg-faint">
                Index signed with {catalog.index.key_id}, generated {date(catalog.index.generated_at)}, fetched {date(catalog.index.fetched_at)}.
              </p>
            {/if}
            {#if !catalog.index}
              <Empty title="The registry has not been fetched yet" description="Refresh to fetch and verify the plugin registry's index." iconName="browser-code" size="sm" />
            {:else if !catalog.plugins.length}
              <Empty title="The registry is empty" iconName="browser-code" size="sm" />
            {:else}
              <div class="flex flex-col gap-4">
                {#each catalog.plugins as c (c.id)}
                  {@const a = action(c)}
                  <div class="rounded-lg border border-neutral-200 p-4 dark:border-white/[0.08]" data-catalog={c.id}>
                    <div class="flex flex-wrap items-start justify-between gap-3">
                      <div class="min-w-0">
                        <div class="flex flex-wrap items-center gap-2">
                          <h4 class="text-[13px] font-semibold text-black dark:text-fg">{c.name}</h4>
                          {#if c.latest_version}<span class="table-badge">v{c.latest_version}</span>{/if}
                          {#if c.installed_version}<span class="table-badge table-badge-success">Installed v{c.installed_version}</span>{/if}
                          {#if c.installed_yanked}<span class="table-badge table-badge-warning" title="The installed version was yanked from the registry">Yanked</span>{/if}
                          {#if c.update_available}<span class="table-badge table-badge-warning">Update available</span>{/if}
                          {#if c.revoked}<span class="table-badge table-badge-danger">Revoked</span>{/if}
                        </div>
                        <p class="mt-0.5 text-[12px] text-neutral-500 dark:text-fg-dim">{c.description}</p>
                        <p class="mt-0.5 text-[11px] text-neutral-500 dark:text-fg-faint">{c.id} · {c.license}</p>
                      </div>
                      {#if a}
                        <button type="button" class="button button-highlighted shrink-0" onclick={() => run(c)}>{a.label}</button>
                      {/if}
                    </div>
                    {#if c.revoked}
                      <Callout type="danger" title="Revoked by the plugin registry" class="mt-3">{c.revoked.reason}</Callout>
                    {:else if c.pending_update}
                      <Callout type="warning" title="Update waiting for approval" class="mt-3">
                        Version {c.pending_update.version} asks for more than was granted: {c.pending_update.added.join('; ')}. The installed version keeps running until you approve it.
                      </Callout>
                    {:else if !c.latest_version}
                      <Callout type="info" title="Nothing to install" class="mt-3">No version of this plugin can run on this coolify-rs.</Callout>
                    {/if}
                    <div class="mt-3">
                      <h5 class="mb-1.5 text-[12px] font-medium text-neutral-700 dark:text-fg-dim">Capabilities</h5>
                      <CapabilityList capabilities={c.capabilities} />
                    </div>
                  </div>
                {/each}
              </div>
            {/if}
          {/if}
        </SettingsSection>

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
  <Modal
    title={approval.title}
    subtitle={approval.plugin ? `${approval.plugin.name} v${approval.plugin.version} (${approval.plugin.id})` : approval.version ? `Version ${approval.version}` : undefined}
    onclose={() => (approval = null)}
    narrow
  >
    {#if approval.unverified}
      <Callout type="warning" title="Unverified" class="mb-4">This plugin is not from the plugin registry. Nobody has reviewed what it does.</Callout>
    {/if}
    {#if approval.plugin?.description}
      <p class="mb-3 text-[12px] text-neutral-600 dark:text-fg-dim">{approval.plugin.description}</p>
    {/if}
    {#if approval.added?.length && approval.added.length < approval.capabilities.length}
      <Callout type="warning" title="This version asks for more" class="mb-4">
        <ul class="list-disc pl-4">
          {#each approval.added as a (a)}<li>{a}</li>{/each}
        </ul>
      </Callout>
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
