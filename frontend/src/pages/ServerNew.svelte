<script lang="ts">
  // Port of upstream livewire/server/create.blade.php with
  // livewire/server/new/by-ip.blade.php. Provisioning through cloud
  // providers is not available in coolify-rs, so only "IP address or domain"
  // is offered.
  import Reicon from '../components/Reicon.svelte';
  import FormInput from '../components/FormInput.svelte';
  import Listbox from '../components/Listbox.svelte';
  import SettingsSection from '../components/SettingsSection.svelte';
  import Unported from './Unported.svelte';
  import { keysUi, serversUi, ApiError } from '../lib/api';
  import { link, router } from '../lib/router.svelte';
  import { session } from '../lib/session.svelte';
  import { notify } from '../lib/toast.svelte';

  let { type }: { type?: string } = $props();

  let keys = $state<{ id: number; name: string }[]>([]);
  let privateKeyId = $state<string>('');
  let name = $state('');
  let description = $state('');
  let ip = $state('');
  let user = $state('root');
  let port = $state('22');
  let serverRole = $state<string>('both');
  let advanced = $state(false);
  let keyMenu = $state(false);
  let keyMenuRoot: HTMLDivElement | undefined = $state();
  let busy = $state(false);
  let error = $state<ApiError | null>(null);

  async function loadForm(selectId?: number) {
    const f = await serversUi.form();
    keys = f.keys;
    if (!name) name = f.name;
    const pick = selectId ?? keys[0]?.id;
    privateKeyId = pick === undefined ? '' : String(pick);
  }
  $effect(() => {
    if (type === 'manual') loadForm();
  });

  async function generate(kind: 'ed25519' | 'rsa') {
    keyMenu = false;
    try {
      await keysUi.generate(kind);
      const before = new Set(keys.map((k) => k.id));
      const f = await serversUi.form();
      keys = f.keys;
      privateKeyId = String(f.keys.find((k) => !before.has(k.id))?.id ?? privateKeyId);
      notify.ok('Private key created successfully.');
    } catch (e) {
      notify.error(e instanceof ApiError ? e.message : String(e));
    }
  }

  async function submit(e: SubmitEvent) {
    e.preventDefault();
    busy = true;
    error = null;
    try {
      const { uuid } = await serversUi.create({
        private_key_id: privateKeyId === '' ? null : Number(privateKeyId),
        name,
        description,
        ip,
        user,
        port: port === '' ? null : Number(port),
        server_role: serverRole,
      });
      router.go(`/server/${uuid}`);
    } catch (err) {
      error = err instanceof ApiError ? err : new ApiError(0, String(err));
      if (!Object.keys(error.errors).length) notify.error(error.message);
      else if (error.field('user') || error.field('port') || error.field('server_role')) advanced = true;
    } finally {
      busy = false;
    }
  }

  const card =
    'group flex min-h-32 flex-col rounded-xl border border-neutral-200 bg-white p-3 shadow-sm transition-all hover:-translate-y-px hover:border-neutral-300 hover:no-underline hover:shadow-md dark:border-white/[0.08] dark:bg-white/[0.05] dark:hover:border-white/[0.14]';
</script>

<svelte:window onclick={(e) => keyMenu && keyMenuRoot && !keyMenuRoot.contains(e.target as Node) && (keyMenu = false)} />

{#if type && type !== 'manual'}
  <Unported />
{:else}
  <div class="w-full">
    <div class="mb-5 flex min-h-9 flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
      <h1 class="min-w-0 text-[24px]! leading-7! font-semibold! tracking-tight!">New server</h1>
      <div class="flex flex-wrap items-center gap-2">
        {#if type}<a href="/servers/new" class="button" use:link>Change method</a>{/if}
      </div>
    </div>

    {#if !type}
      <div class="application-settings-form flex flex-col gap-6">
        <section class="application-settings-section">
          <div class="application-settings-section-header">
            <h2 class="application-settings-section-title">Add a server</h2>
            <p class="application-settings-section-description">Connect a server you already manage.</p>
          </div>
          <div class="application-settings-section-body is-flush">
            <div class="grid grid-cols-1 gap-3 p-3 sm:grid-cols-2 lg:grid-cols-4">
              <a href="/servers/new/manual" use:link class={card}>
                <div class="flex items-start">
                  <span
                    class="flex size-8 items-center justify-center rounded-lg border border-neutral-200 bg-neutral-50 text-neutral-500 dark:border-white/[0.08] dark:bg-white/[0.04] dark:text-fg-dim"
                  >
                    <Reicon name="servers" class="size-4" />
                  </span>
                </div>
                <div class="mt-auto pt-5">
                  <h3 class="text-[13px]! font-semibold! text-black dark:text-fg">IP address or domain</h3>
                  <p class="mt-1 text-[11px] leading-4 text-neutral-500 dark:text-fg-faint">Connect an existing server over SSH.</p>
                </div>
              </a>
            </div>
          </div>
        </section>
      </div>
    {:else}
      <div class="application-settings-form">
        <div class="w-full">
          <form onsubmit={submit}>
            <SettingsSection title="Connect a server" description="Add an existing Linux server using its SSH connection details.">
              {#snippet actions()}
                <button type="submit" class="button button-highlighted" disabled={busy}>
                  Continue
                  <Reicon name="arrow-right" class="size-3.5" />
                </button>
              {/snippet}
              <div class="mb-5">
                <FormInput id="ip" label="IP address or domain" required helper="For example 127.0.0.1 or server.example.com." bind:value={ip} error={error?.field('ip')} />
              </div>
              <div class="mb-5">
                <div class="flex items-end gap-3">
                  <div class="min-w-0 flex-1">
                    <Listbox
                      id="private_key_id"
                      label="Private key"
                      placeholder="Select a private key"
                      bind:value={privateKeyId}
                      options={keys.map((k) => ({ value: String(k.id), label: k.name }))}
                    />
                    {#if error?.field('private_key_id')}
                      <label class="label" for="private_key_id-trigger"><span class="text-red-500 label-text-alt">{error.field('private_key_id')}</span></label>
                    {/if}
                  </div>
                  {#if session.isAdmin}
                    <div class="relative shrink-0" bind:this={keyMenuRoot}>
                      <button type="button" class="button" onclick={() => (keyMenu = !keyMenu)} aria-haspopup="menu" aria-expanded={keyMenu}>
                        <Reicon name="plus" class="size-3.5" />
                        New key
                        <Reicon name="chevron-down" class="size-3 opacity-55" />
                      </button>
                      {#if keyMenu}
                        <div role="menu" class="listbox-panel left-auto! right-0! z-[90]! w-52! min-w-52!">
                          <button type="button" class="listbox-option justify-start! gap-2.5!" onclick={() => generate('ed25519')} role="menuitem">
                            <Reicon name="keys" class="size-3.5 shrink-0 opacity-70" />
                            Generate ED25519
                          </button>
                          <button type="button" class="listbox-option justify-start! gap-2.5!" onclick={() => generate('rsa')} role="menuitem">
                            <Reicon name="keys" class="size-3.5 shrink-0 opacity-70" />
                            Generate RSA
                          </button>
                          <a href="/security/private-key" use:link class="listbox-option justify-start! gap-2.5!" role="menuitem">
                            <Reicon name="plus" class="size-3.5 shrink-0 opacity-70" />
                            Add manually
                          </a>
                        </div>
                      {/if}
                    </div>
                  {/if}
                </div>
              </div>
              <div class="grid gap-4 border-t border-neutral-200 pt-4 lg:grid-cols-2 dark:border-white/[0.08]">
                <FormInput id="name" label="Name" required bind:value={name} error={error?.field('name')} />
                <FormInput id="description" label="Description" bind:value={description} error={error?.field('description')} />
              </div>
              <div class="flex flex-col gap-4 mt-5 border-t border-neutral-200 pt-4 dark:border-white/[0.08]">
                <button
                  type="button"
                  onclick={() => (advanced = !advanced)}
                  class="flex items-center gap-2 text-left text-sm font-medium hover:underline"
                  aria-expanded={advanced}
                >
                  <svg class="size-4 transition-transform {advanced ? 'rotate-90' : ''}" viewBox="0 0 20 20" fill="currentColor" aria-hidden="true">
                    <path
                      fill-rule="evenodd"
                      d="M7.21 14.77a.75.75 0 0 1 .02-1.06L11.168 10 7.23 6.29a.75.75 0 1 1 1.04-1.08l4.5 4.25a.75.75 0 0 1 0 1.08l-4.5 4.25a.75.75 0 0 1-1.06-.02Z"
                      clip-rule="evenodd"
                    />
                  </svg>
                  Advanced settings
                </button>
                {#if advanced}
                  <div class="rounded-lg border border-neutral-200 p-4 dark:border-coolgray-400 flex flex-col gap-4">
                    <div class="grid gap-4 lg:grid-cols-2">
                      <FormInput id="user" label="User" required helper="Non-root SSH users are experimental." bind:value={user} error={error?.field('user')} />
                      <FormInput type="number" id="port" label="Port" required bind:value={port} error={error?.field('port')} />
                    </div>
                    <Listbox
                      id="server_role"
                      label="Server role"
                      helper="Choose whether this server runs deployments, application builds, or both. GitHub Actions runners need the Builds only or the Deployments and builds role."
                      bind:value={serverRole}
                      options={[
                        { value: 'deployment', label: 'Deployments only', description: 'Runs your resources. Images are built on a build server.' },
                        { value: 'build', label: 'Builds only', description: 'Builds images for other servers and runs GitHub Actions runners.' },
                        { value: 'both', label: 'Deployments and builds', description: 'Builds and runs your resources on this server. Can also run GitHub Actions runners.' },
                      ]}
                    />
                  </div>
                {/if}
              </div>
            </SettingsSection>
          </form>
        </div>
      </div>
    {/if}
  </div>
{/if}
