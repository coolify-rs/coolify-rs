<script lang="ts">
  // Port of upstream livewire/boarding/index.blade.php (App\Livewire\Boarding\Index) with
  // components/boarding-progress.blade.php and components/boarding-step.blade.php. The Hetzner, Vultr,
  // DigitalOcean and Hostinger cards open `Server\New\By*` in a modal. Without `selectedTokenUuid` those stop at
  // step 1, whose Continue goes to `server.create.token`, so upstream's `from_onboarding` branch never runs.
  import type { Snippet } from 'svelte';
  import CheckpointItem from '../components/CheckpointItem.svelte';
  import FormInput from '../components/FormInput.svelte';
  import FormTextarea from '../components/FormTextarea.svelte';
  import HelpForm from '../components/HelpForm.svelte';
  import Listbox from '../components/Listbox.svelte';
  import Modal from '../components/Modal.svelte';
  import ProcessDialog from '../components/ProcessDialog.svelte';
  import Reicon from '../components/Reicon.svelte';
  import SettingsSection from '../components/SettingsSection.svelte';
  import ValidateServer from '../components/ValidateServer.svelte';
  import ServerNewDigitalOcean from './ServerNewDigitalOcean.svelte';
  import ServerNewHetzner from './ServerNewHetzner.svelte';
  import ServerNewHostinger from './ServerNewHostinger.svelte';
  import ServerNewVultr from './ServerNewVultr.svelte';
  import { ApiError, request, serversUi, type ValidationProgress } from '../lib/api';
  import { router } from '../lib/router.svelte';
  import { session } from '../lib/session.svelte';
  import { notify } from '../lib/toast.svelte';

  document.title = 'Onboarding | coolify-rs';

  type Option = { value: number; label: string };
  type Server = { id: number; uuid: string; name: string; ip: string; port: number; user: string };

  // `#[Url]` properties.
  let currentState = $state(router.query('step') ?? 'welcome');
  let selectedServerType = $state<string | null>(router.query('selectedServerType'));
  const urlInt = (k: string) => {
    const v = router.query(k);
    return v === null || v === '' ? null : Number(v);
  };
  let selectedExistingPrivateKey = $state<number | null>(urlInt('selectedExistingPrivateKey'));
  let privateKeyType = $state<string | null>(router.query('privateKeyType'));
  let selectedExistingServer = $state<number | null>(urlInt('selectedExistingServer'));
  let selectedProject = $state<number | null>(urlInt('selectedProject'));

  let privateKeys = $state<Option[]>([]);
  let projects = $state<Option[]>([]);
  let privateKey = $state('');
  let publicKey = $state('');
  let privateKeyName = $state('');
  let privateKeyDescription = $state('');
  let createdPrivateKey = $state<number | null>(null);
  let remoteServerName = $state('');
  let remoteServerDescription = $state('');
  let remoteServerHost = $state('');
  let remoteServerPort = $state('22');
  let remoteServerUser = $state('root');
  let createdServer = $state<Server | null>(null);
  let createdProject = $state<{ id: number; name: string } | null>(null);
  let serverPublicKey = $state('');
  let serverReachable = $state(true);
  let minDockerVersion = $state('24');
  let errors = $state<Record<string, string[]>>({});
  let helpOpen = $state(false);
  let advancedOpen = $state(false);
  let validating = $state(false);
  let progress = $state<ValidationProgress | null>(null);
  let loaded = $state(false);

  /** `#[Url(as: 'step', history: true)]` and the `keep: true` parameters. */
  $effect(() => {
    const q = new URLSearchParams();
    q.set('selectedServerType', selectedServerType ?? '');
    q.set('selectedExistingPrivateKey', selectedExistingPrivateKey === null ? '' : String(selectedExistingPrivateKey));
    q.set('privateKeyType', privateKeyType ?? '');
    q.set('selectedExistingServer', selectedExistingServer === null ? '' : String(selectedExistingServer));
    q.set('selectedProject', selectedProject === null ? '' : String(selectedProject));
    q.set('step', currentState);
    if (loaded && currentState !== 'welcome') router.go(`/onboarding?${q}`);
  });

  function applyGenerated(g: { name: string; description: string; private_key: string; public_key: string }) {
    privateKeyName = g.name;
    privateKeyDescription = g.description;
    privateKey = g.private_key;
    publicKey = g.public_key;
  }

  async function load() {
    const d = await request<any>('GET', `/ui/onboarding${router.search}`);
    if (d.redirect) {
      router.go(d.redirect, true);
      return;
    }
    minDockerVersion = d.min_docker_version;
    privateKeyName = d.private_key_name;
    remoteServerName = d.server_name;
    privateKeys = d.private_keys;
    projects = d.projects;
    createdServer = d.server;
    serverPublicKey = d.server_public_key ?? '';
    createdProject = d.created_project;
    if (d.generated) applyGenerated(d.generated);
    if (createdServer) {
      remoteServerPort = String(createdServer.port);
      remoteServerUser = createdServer.user;
    }
    loaded = true;
  }
  load();

  function apply(r: any) {
    if (r.errors) errors = r.errors;
    if (r.private_keys) privateKeys = r.private_keys;
    if ('selected_private_key' in r) selectedExistingPrivateKey = r.selected_private_key;
    if (r.generated) applyGenerated(r.generated);
    if (r.created_private_key) createdPrivateKey = r.created_private_key;
    if (r.server) createdServer = r.server;
    if (r.server_public_key) serverPublicKey = r.server_public_key;
    if ('selected_server' in r && r.selected_server !== null) selectedExistingServer = r.selected_server;
    if ('server_reachable' in r) serverReachable = r.server_reachable;
    if (r.projects) projects = r.projects;
    if ('selected_project' in r) selectedProject = r.selected_project;
    if (r.created_project) createdProject = r.created_project;
    if (r.state) currentState = r.state;
    if (r.error) notify.failure(r.error);
  }

  async function run(name: string, values: Record<string, unknown> = {}) {
    errors = {};
    try {
      const r = await request<any>('POST', '/ui/onboarding', { action: name, values });
      if (r.redirect) {
        // `skipBoarding` first: the session no longer sends the user back here.
        await session.refresh();
        router.go(r.redirect);
        return r;
      }
      apply(r);
      return r;
    } catch (e) {
      notify.failure(e instanceof ApiError ? e.message : String(e));
      return null;
    }
  }

  /** `explanation`. */
  const explanation = () => (currentState = 'select-server-type');

  /** `skipBoarding`. */
  async function skipBoarding() {
    await request('POST', '/ui/onboarding', { action: 'skipBoarding', values: {} });
    await session.refresh();
    router.go('/', true);
  }

  /** `restartBoarding`: `redirect()->route('onboarding')`. */
  function restart() {
    window.location.assign('/onboarding');
  }

  function setServerType(type: string) {
    selectedServerType = type;
    if (type === 'localhost') selectedExistingServer = 0;
    run('setServerType', { type });
  }

  function setPrivateKey(type: string) {
    selectedExistingPrivateKey = null;
    privateKeyType = type;
    if (type !== 'create') {
      privateKey = '';
      publicKey = '';
    }
    run('setPrivateKey', { type });
  }

  async function startValidation() {
    if (!createdServer) return;
    progress = null;
    validating = true;
    try {
      await serversUi.validate(createdServer.uuid);
    } catch (e) {
      validating = false;
      notify.failure(e instanceof ApiError ? e.message : String(e));
      return;
    }
    watch();
  }

  /** `refreshBoardingIndex` when `validate-and-install` finishes. */
  async function watch() {
    if (!createdServer) return;
    const v = await serversUi.validation(createdServer.uuid);
    progress = v.progress;
    if (v.progress?.running) {
      setTimeout(watch, 1000);
      return;
    }
    if (v.progress && !v.progress.error) {
      validating = false;
      notify.success('Server validated, proxy is starting in a moment.');
      run('validateServer', { serverId: createdServer.id });
    }
  }

  const keyName = (k: string) => (k === 'privateKey' ? errors.privateKey?.[0] ?? errors.private_key?.[0] : errors[k]?.[0]);

  const pillButton =
    'inline-flex items-center gap-1.5 rounded-md px-3 py-1.5 text-[12px] font-medium text-neutral-500 transition-colors hover:bg-white hover:text-coollabs dark:text-fg-dim dark:hover:bg-white/[0.06] dark:hover:text-brand-light';
  const pillGroup = 'inline-flex flex-wrap items-center justify-center gap-0.5 rounded-lg border border-neutral-200 bg-neutral-50 p-0.5 dark:border-white/[0.08] dark:bg-white/[0.05]';
  const card =
    'group relative cursor-pointer min-h-36 rounded-[10px] border border-neutral-200 bg-white p-4 text-left shadow-sm transition-all hover:-translate-y-px hover:border-neutral-300 hover:shadow-md dark:border-white/[0.08] dark:bg-white/[0.05] dark:hover:border-white/[0.14]';
  let provision = $state<'hetzner' | 'vultr' | 'digital-ocean' | 'hostinger' | null>(null);
  const providerTitle = { hetzner: 'Hetzner', vultr: 'Vultr', 'digital-ocean': 'DigitalOcean', hostinger: 'Hostinger' };
  const providerCard =
    'group relative cursor-pointer flex h-full min-h-36 flex-col rounded-[10px] border border-neutral-200 bg-white p-4 text-left shadow-sm transition-all hover:-translate-y-px hover:border-neutral-300 hover:shadow-md dark:border-white/[0.08] dark:bg-white/[0.05] dark:hover:border-white/[0.14]';
  const keyCard =
    'group flex h-full min-h-28 items-start gap-3 rounded-[10px] border border-neutral-200 bg-white p-4 text-left shadow-sm transition-all hover:-translate-y-px hover:border-neutral-300 hover:shadow-md dark:border-white/[0.08] dark:bg-white/[0.05] dark:hover:border-white/[0.14]';
  const keyIcon = 'flex size-9 shrink-0 items-center justify-center rounded-lg border border-neutral-200 bg-neutral-50 text-neutral-500 dark:border-white/[0.08] dark:bg-white/[0.035] dark:text-fg-dim';
  const info =
    'absolute top-3 right-3 flex size-6 items-center justify-center rounded-full border border-neutral-200 text-[11px] font-semibold text-neutral-500 hover:border-coollabs/35 hover:text-coollabs dark:border-white/[0.1] dark:text-fg-dim dark:hover:border-brand-light/30 dark:hover:text-brand-light';
  const code = 'text-xs bg-coolgray-300 dark:bg-coolgray-400 px-1 py-0.5 rounded';
</script>

{#snippet progressBar(currentStep: number)}
  <div class="mx-auto mb-6 w-full max-w-3xl">
    <div class="grid grid-cols-3 overflow-hidden rounded-[10px] border border-neutral-200 bg-white dark:border-white/[0.08] dark:bg-white/[0.05]">
      {#each [1, 2, 3] as i}
        <div
          class="flex min-h-10 items-center justify-center gap-2 border-r border-neutral-200 px-3 text-[11px] font-medium last:border-r-0 dark:border-white/[0.08] {i === currentStep
            ? 'bg-coollabs/[0.07] text-coollabs dark:bg-brand/10 dark:text-brand-light'
            : 'text-neutral-500 dark:text-fg-dim'}"
        >
          <span
            class="flex size-5 items-center justify-center rounded-full border text-[10px] font-semibold {i === currentStep
              ? 'border-coollabs/25 bg-coollabs/10 dark:border-brand-light/25 dark:bg-brand/15'
              : i < currentStep
                ? 'border-emerald-500/25 bg-emerald-500/10 text-emerald-600 dark:text-emerald-400'
                : 'border-neutral-200 dark:border-white/[0.1]'}"
          >
            {#if i < currentStep}
              <Reicon name="check-circle" class="size-3" />
            {:else}
              {i}
            {/if}
          </span>
          <span class="hidden sm:inline">{i === 1 ? 'Server' : i === 2 ? 'Connection' : 'Complete'}</span>
        </div>
      {/each}
    </div>
  </div>
{/snippet}

{#snippet step(title: string, question: string, actions: Snippet, explanationBody?: Snippet)}
  <div class="application-settings-form w-full max-w-3xl">
    <section class="application-settings-section">
      <div class="application-settings-section-header">
        <div>
          <h2>{title}</h2>
          <p>{question}</p>
        </div>
      </div>
      <div class="application-settings-section-body">
        <div class="flex flex-col gap-4">
          {@render actions()}
        </div>
      </div>
      {#if explanationBody}
        <div class="border-t border-neutral-200 bg-neutral-50 px-4 py-3 dark:border-white/[0.08] dark:bg-white/[0.02]">
          <p class="mb-2 text-[10px] font-semibold uppercase tracking-[0.08em] text-neutral-400 dark:text-fg-faint">Technical details</p>
          <div class="space-y-2 text-[12px] leading-5 text-neutral-500 dark:text-fg-dim">
            {@render explanationBody()}
          </div>
        </div>
      {/if}
    </section>
  </div>
{/snippet}

{#snippet help()}
  <button type="button" class={pillButton} onclick={() => (helpOpen = true)}>
    <Reicon name="feedback" class="size-3.5 shrink-0" />
    Contact support
  </button>
{/snippet}

{#snippet high(text: string)}<span class="text-helper">{text}</span>{/snippet}

<main class="min-h-screen flex items-center justify-center p-4">
  {#if loaded}
    <section class="application-settings-form w-full py-6">
      <div class="flex w-full flex-col items-center space-y-6">
        {#if currentState === 'welcome'}
          <div class="w-full max-w-3xl">
            <div class="mb-6 text-center">
              <h1 class="text-2xl! font-semibold!">Welcome to coolify-rs</h1>
              <p class="mt-1 text-[13px] text-neutral-500 dark:text-fg-dim">Connect your first server and start deploying in minutes.</p>
            </div>
            <SettingsSection title="What you will set up" flush>
              <div class="divide-y divide-neutral-200 dark:divide-white/[0.07]">
                {#each [{ icon: 'servers', title: 'Server connection', description: 'Connect through SSH to host your resources.' }, { icon: 'settings', title: 'Docker environment', description: 'Validate and configure the deployment runtime.' }, { icon: 'projects', title: 'Project structure', description: 'Create a project and its first environment.' }] as item}
                  <div class="flex min-h-14 items-center gap-3 px-4 py-3">
                    <span class="flex size-8 shrink-0 items-center justify-center rounded-lg border border-neutral-200 bg-neutral-50 text-neutral-500 dark:border-white/[0.08] dark:bg-white/[0.035] dark:text-fg-dim">
                      <Reicon name={item.icon} class="size-4" />
                    </span>
                    <span class="min-w-0">
                      <span class="block text-[13px] font-semibold">{item.title}</span>
                      <span class="mt-0.5 block text-[11px] text-neutral-500 dark:text-fg-faint">{item.description}</span>
                    </span>
                  </div>
                {/each}
              </div>
            </SettingsSection>
            <div class="mt-5 flex flex-col items-center gap-4">
              <button type="button" class="button button-highlighted w-full justify-center sm:w-auto sm:min-w-36" onclick={explanation}>Continue</button>
              <div class={pillGroup}>
                <button type="button" class={pillButton} onclick={skipBoarding}>
                  <Reicon name="arrow-right" class="size-3.5 shrink-0" />
                  Skip setup
                </button>
                {@render help()}
              </div>
            </div>
          </div>
        {:else if currentState === 'select-server-type'}
          {@render progressBar(1)}
          {#snippet serverTypeActions()}
            <div class="w-full space-y-6">
              <section>
                <h3 class="text-base font-semibold">Add a server</h3>
                <p class="mb-3 text-sm text-neutral-500 dark:text-neutral-400">Use this machine or connect a server you already manage.</p>
                <div class="grid w-full grid-cols-1 gap-4 lg:grid-cols-2">
                  <button class={card} onclick={() => setServerType('localhost')}>
                    <span role="button" tabindex="-1" aria-label="About this machine" title="The machine running coolify-rs. Not recommended for production workloads due to resource contention." class={info}>i</span>
                    <div class="flex flex-col gap-4 text-left">
                      <svg class="size-10" xmlns="http://www.w3.org/2000/svg" fill="none" viewBox="0 0 24 24" stroke-width="1.5" stroke="currentColor">
                        <path
                          stroke-linecap="round"
                          stroke-linejoin="round"
                          d="M5.25 14.25h13.5m-13.5 0a3 3 0 01-3-3m3 3a3 3 0 100 6h13.5a3 3 0 100-6m-16.5-3a3 3 0 013-3h13.5a3 3 0 013 3m-19.5 0a4.5 4.5 0 01.9-2.7L5.737 5.1a3.375 3.375 0 012.7-1.35h7.126c1.062 0 2.062.5 2.7 1.35l2.587 3.45a4.5 4.5 0 01.9 2.7m0 0a3 3 0 01-3 3m0 3h.008v.008h-.008v-.008zm0-6h.008v.008h-.008v-.008zm-3 6h.008v.008h-.008v-.008zm0-6h.008v.008h-.008v-.008z"
                        />
                      </svg>
                      <div>
                        <h3 class="mb-1 text-[14px] font-semibold">This machine</h3>
                        <p class="text-sm dark:text-neutral-400">Deploy on the server running coolify-rs. Best for testing and single-server setups.</p>
                      </div>
                    </div>
                  </button>
                  <button class={card} onclick={() => setServerType('remote')}>
                    <span role="button" tabindex="-1" aria-label="About remote servers" title="Any SSH-accessible server, including cloud VPS, bare metal, and self-hosted infrastructure." class={info}>i</span>
                    <div class="flex flex-col gap-4 text-left">
                      <svg class="size-10" xmlns="http://www.w3.org/2000/svg" fill="none" viewBox="0 0 24 24" stroke-width="1.5" stroke="currentColor">
                        <path stroke-linecap="round" stroke-linejoin="round" d="M2.25 15a4.5 4.5 0 004.5 4.5H18a3.75 3.75 0 001.332-7.257 3 3 0 00-3.758-3.848 5.25 5.25 0 00-10.233 2.33A4.502 4.502 0 002.25 15z" />
                      </svg>
                      <div>
                        <h3 class="mb-1 text-[14px] font-semibold">IP address or domain</h3>
                        <p class="text-sm dark:text-neutral-400">Connect via SSH using a server IP address or domain.</p>
                      </div>
                    </div>
                  </button>
                </div>
              </section>
              {#if session.isAdmin}
              <section>
                <h3 class="text-base font-semibold">Provision a server</h3>
                <p class="mb-3 text-sm text-neutral-500 dark:text-neutral-400">Create a server with a cloud provider.</p>
                <div class="grid w-full grid-cols-1 gap-4 lg:grid-cols-2">
                  <div class={providerCard} role="button" tabindex="0" onclick={() => (provision = 'hetzner')} onkeydown={(e) => e.key === 'Enter' && (provision = 'hetzner')}>
                    <div class="flex h-full flex-col gap-4 text-left">
                      <img src="/svgs/hetzner.svg" alt="Hetzner" class="size-10 shrink-0" />
                      <div class="min-h-0 flex-1">
                        <h3 class="mb-1 text-[14px] font-semibold">Hetzner Cloud</h3>
                        <p class="text-sm dark:text-neutral-400">Deploy servers directly from your Hetzner Cloud account.</p>
                      </div>
                    </div>
                  </div>
                  <div class={providerCard} role="button" tabindex="0" onclick={() => (provision = 'vultr')} onkeydown={(e) => e.key === 'Enter' && (provision = 'vultr')}>
                    <div class="flex h-full flex-col gap-4 text-left">
                      <img src="https://www.vultr.com/media/logo_ondark.svg" alt="Vultr" class="h-10 w-28 shrink-0 object-contain object-left" />
                      <div class="min-h-0 flex-1">
                        <h3 class="mb-1 text-[14px] font-semibold">Vultr Cloud</h3>
                        <p class="text-sm dark:text-neutral-400">Deploy servers directly from your Vultr account.</p>
                      </div>
                    </div>
                  </div>
                  <div class={providerCard} role="button" tabindex="0" onclick={() => (provision = 'digital-ocean')} onkeydown={(e) => e.key === 'Enter' && (provision = 'digital-ocean')}>
                    <div class="flex h-full flex-col gap-4 text-left">
                      <svg class="size-10 shrink-0" fill="#0080FF" role="img" viewBox="0 0 24 24" xmlns="http://www.w3.org/2000/svg">
                        <title>DigitalOcean</title>
                        <path
                          d="M12.04 0C5.408-.02.005 5.37.005 11.992h4.638c0-4.923 4.882-8.731 10.064-6.855a6.95 6.95 0 014.147 4.148c1.889 5.177-1.924 10.055-6.84 10.064v-4.61H7.391v4.623h4.61V24c7.86 0 13.967-7.588 11.397-15.83-1.115-3.59-3.985-6.446-7.575-7.575A12.8 12.8 0 0012.039 0zM7.39 19.362H3.828v3.564H7.39zm-3.563 0v-2.978H.85v2.978z"
                        />
                      </svg>
                      <div class="min-h-0 flex-1">
                        <h3 class="mb-1 text-[14px] font-semibold">DigitalOcean</h3>
                        <p class="text-sm dark:text-neutral-400">Deploy servers directly from your DigitalOcean account.</p>
                      </div>
                    </div>
                  </div>
                  <div class="{providerCard} dark:bg-white/[0.025]" role="button" tabindex="0" onclick={() => (provision = 'hostinger')} onkeydown={(e) => e.key === 'Enter' && (provision = 'hostinger')}>
                    <div class="flex h-full flex-col gap-4 text-left">
                      <svg class="size-10 shrink-0 text-[#673de6]" viewBox="0 0 26 30" fill="none" xmlns="http://www.w3.org/2000/svg" role="img" aria-label="Hostinger">
                        <title>Hostinger</title>
                        <path
                          fill-rule="evenodd"
                          clip-rule="evenodd"
                          d="M0.000249566 14.046V0.000497794L6.99464 3.78046V10.1086L16.2538 10.1132L23.3617 14.046H0.000249566ZM18.1472 8.95059V0L25.3302 3.68591V13.1797L18.1472 8.95059ZM18.1472 26.1177V19.8441L8.81663 19.8376C8.82534 19.8793 1.59081 15.8418 1.59081 15.8418L25.3302 15.9547V30L18.1472 26.1177ZM0 26.1177L0.000248849 16.9393L6.99464 21.0683V29.8033L0 26.1177Z"
                          fill="currentColor"
                        />
                      </svg>
                      <div class="min-h-0 flex-1">
                        <h3 class="mb-1 text-[14px] font-semibold">Hostinger</h3>
                        <p class="text-sm dark:text-neutral-400">Deploy servers directly from your Hostinger account.</p>
                      </div>
                    </div>
                  </div>
                </div>
              </section>
              {/if}
            </div>
            {#if !serverReachable}
              <div class="mt-6 p-4 border border-error rounded-lg text-gray-800 dark:text-gray-200">
                <h2 class="text-lg font-bold mb-2">Server is not reachable</h2>
                <p class="mb-4">Please check the connection details below and correct them if they are incorrect.</p>
                <div class="grid grid-cols-1 md:grid-cols-2 gap-4 mb-4">
                  <FormInput placeholder="Default is 22" label="Port" id="remoteServerPort" error={errors.remoteServerPort?.[0]} bind:value={remoteServerPort} />
                  <div>
                    <FormInput placeholder="Default is root" label="User" id="remoteServerUser" error={errors.remoteServerUser?.[0]} bind:value={remoteServerUser} />
                    <p class="text-xs mt-1">
                      Non-root user is experimental:
                      <a class="font-bold underline" target="_blank" href="https://coolify.io/docs/knowledge-base/server/non-root-user">docs</a>
                    </p>
                  </div>
                </div>
                <div class="mb-4">
                  <p class="mb-2">If the connection details are correct, please ensure:</p>
                  <ul class="list-disc list-inside">
                    <li>The correct public key is in your <code class="bg-red-200 dark:bg-red-900 px-1 rounded-sm">~/.ssh/authorized_keys</code> file for the specified user</li>
                    <li>Or skip the boarding process and manually add a new private key to coolify-rs and the server</li>
                  </ul>
                </div>
                <p class="mb-4">
                  For more help, check this <a target="_blank" class="underline font-semibold" href="https://coolify.io/docs/knowledge-base/server/openssh">documentation</a>.
                </p>
                <div class="mb-4">
                  <FormInput readonly id="serverPublicKey" label="Current Public Key" value={serverPublicKey} />
                </div>
                <button type="button" class="button button-highlighted w-full justify-center" onclick={() => run('saveAndValidateServer', { serverId: createdServer?.id, remoteServerPort, remoteServerUser })}>Check again</button>
              </div>
            {/if}
          {/snippet}
          {@render step('Choose Server Type', 'Select where to deploy your applications and databases. You can add more servers later.', serverTypeActions)}
        {:else if currentState === 'private-key'}
          {@render progressBar(2)}
          {#snippet keyActions()}
            {#if privateKeys.length > 0}
              <div class="w-full space-y-4">
                <div class="rounded-[10px] border border-neutral-200 bg-neutral-50 p-4 dark:border-white/[0.08] dark:bg-white/[0.05]">
                  <form
                    class="flex flex-col gap-3 sm:flex-row sm:items-end"
                    onsubmit={(e) => {
                      e.preventDefault();
                      run('selectExistingPrivateKey', { id: selectedExistingPrivateKey });
                    }}
                  >
                    <div class="min-w-0 flex-1">
                      <Listbox id="selectedExistingPrivateKey" label="Existing SSH key" options={privateKeys.map((k) => ({ value: String(k.value), label: k.label }))} value={String(selectedExistingPrivateKey ?? '')} onchange={(v) => (selectedExistingPrivateKey = Number(v))} />
                    </div>
                    <button type="submit" class="button">Use selected key</button>
                  </form>
                </div>
                <div class="relative py-1">
                  <div class="absolute inset-0 flex items-center" aria-hidden="true">
                    <div class="w-full border-t border-neutral-200 dark:border-white/[0.07]"></div>
                  </div>
                  <div class="relative flex justify-center">
                    <span class="bg-[var(--coollabs-base)] px-2.5 text-[10px] font-semibold uppercase tracking-[0.08em] text-neutral-400 dark:text-fg-faint">Or</span>
                  </div>
                </div>
              </div>
            {/if}
            <div class="grid w-full grid-cols-1 gap-3 lg:grid-cols-2">
              <button type="button" class={keyCard} onclick={() => setPrivateKey('own')}>
                <span class={keyIcon}><Reicon name="keys" class="size-4" /></span>
                <span class="min-w-0">
                  <span class="block text-[13px] font-semibold">Use existing key</span>
                  <span class="mt-0.5 block text-[12px] leading-5 text-neutral-500 dark:text-fg-dim">Paste a private key you already manage.</span>
                </span>
              </button>
              <button type="button" class={keyCard} onclick={() => setPrivateKey('create')}>
                <span class={keyIcon}><Reicon name="plus" class="size-4" /></span>
                <span class="min-w-0">
                  <span class="block text-[13px] font-semibold">Generate new key</span>
                  <span class="mt-0.5 block text-[12px] leading-5 text-neutral-500 dark:text-fg-dim">Create an ED25519 key pair in coolify-rs.</span>
                </span>
              </button>
            </div>
          {/snippet}
          {#snippet keyDetails()}
            <p>{@render high('SSH Key Authentication:')} Uses public-key cryptography for secure, password-less server access.</p>
            <p>{@render high('Public Key Deployment:')} Add the public key to your server's <code class={code}>~/.ssh/authorized_keys</code> file.</p>
            <p>{@render high('Key Generation:')} coolify-rs generates ED25519 keys by default for optimal security and performance.</p>
          {/snippet}
          {@render step('SSH Authentication', 'Configure SSH key-based authentication for secure server access.', keyActions, keyDetails)}
        {:else if currentState === 'create-private-key'}
          {@render progressBar(2)}
          {#snippet createKeyActions()}
            <form
              class="flex flex-col w-full gap-4"
              onsubmit={(e) => {
                e.preventDefault();
                run('savePrivateKey', { privateKeyName, privateKeyDescription, privateKey });
              }}
            >
              <FormInput required placeholder="e.g., production-server-key" label="Key Name" id="privateKeyName" error={keyName('privateKeyName')} bind:value={privateKeyName} />
              <FormInput placeholder="Optional: Note what this key is used for" label="Description" id="privateKeyDescription" error={keyName('privateKeyDescription')} bind:value={privateKeyDescription} />
              {#if privateKeyType === 'create'}
                <FormTextarea required readonly label="Private Key" id="privateKey" rows={8} bind:value={privateKey} />
                <FormTextarea rows={7} readonly label="Public Key" id="publicKey" bind:value={publicKey} />
                <div class="p-4 bg-warning/10 border border-warning rounded-lg">
                  <div class="flex gap-3">
                    <svg class="size-5 text-warning flex-shrink-0 mt-0.5" xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 20" fill="currentColor">
                      <path
                        fill-rule="evenodd"
                        d="M8.485 2.495c.673-1.167 2.357-1.167 3.03 0l6.28 10.875c.673 1.167-.17 2.625-1.516 2.625H3.72c-1.347 0-2.189-1.458-1.515-2.625L8.485 2.495zM10 5a.75.75 0 01.75.75v3.5a.75.75 0 01-1.5 0v-3.5A.75.75 0 0110 5zm0 9a1 1 0 100-2 1 1 0 000 2z"
                        clip-rule="evenodd"
                      />
                    </svg>
                    <div>
                      <p class="font-bold text-warning mb-1">Action Required</p>
                      <p class="text-sm dark:text-white text-black">
                        Copy the public key above and add it to your server's
                        <code class={code}>~/.ssh/authorized_keys</code> file.
                      </p>
                    </div>
                  </div>
                </div>
              {:else}
                <FormTextarea required placeholder="-----BEGIN OPENSSH PRIVATE KEY-----" label="Private Key" id="privateKey" rows={8} error={keyName('privateKey')} bind:value={privateKey} />
              {/if}
              <button type="submit" class="button w-full lg:w-auto">Save SSH Key</button>
            </form>
          {/snippet}
          {#snippet createKeyDetails()}
            <p>{@render high('Key Storage:')} Private keys are encrypted at rest in coolify-rs's database.</p>
            <p>{@render high('Public Key Distribution:')} Deploy the public key to <code class={code}>~/.ssh/authorized_keys</code> on your target server for the specified user.</p>
            <p>{@render high('Key Format:')} Supports RSA, ED25519, ECDSA, and DSA key types in OpenSSH format.</p>
          {/snippet}
          {@render step('SSH Key Configuration', 'Configure your SSH key for server authentication.', createKeyActions, createKeyDetails)}
        {:else if currentState === 'create-server'}
          {@render progressBar(2)}
          {#snippet createServerActions()}
            <form
              class="flex flex-col w-full gap-4"
              onsubmit={(e) => {
                e.preventDefault();
                run('saveServer', { remoteServerName, remoteServerHost, remoteServerDescription, remoteServerPort, remoteServerUser, privateKeyId: createdPrivateKey ?? selectedExistingPrivateKey });
              }}
            >
              <div class="grid grid-cols-1 lg:grid-cols-2 gap-4">
                <FormInput required placeholder="e.g., production-app-server" label="Server Name" id="remoteServerName" error={errors.remoteServerName?.[0]} bind:value={remoteServerName} />
                <FormInput required placeholder="IP address or hostname" label="IP Address/Hostname" id="remoteServerHost" error={errors.remoteServerHost?.[0]} bind:value={remoteServerHost} />
              </div>
              <FormInput placeholder="Optional: Note what this server hosts" label="Description" id="remoteServerDescription" bind:value={remoteServerDescription} />
              <div class="flex flex-col gap-4">
                <button type="button" onclick={() => (advancedOpen = !advancedOpen)} class="flex items-center gap-2 text-left text-sm font-medium hover:underline" aria-expanded={advancedOpen}>
                  <svg class="size-4 transition-transform {advancedOpen ? 'rotate-90' : ''}" viewBox="0 0 20 20" fill="currentColor" aria-hidden="true">
                    <path fill-rule="evenodd" d="M7.21 14.77a.75.75 0 0 1 .02-1.06L11.168 10 7.23 6.29a.75.75 0 1 1 1.04-1.08l4.5 4.25a.75.75 0 0 1 0 1.08l-4.5 4.25a.75.75 0 0 1-1.06-.02Z" clip-rule="evenodd" />
                  </svg>
                  Advanced Settings
                </button>
                {#if advancedOpen}
                  <div class="rounded-lg border border-neutral-200 p-4 dark:border-coolgray-400 grid grid-cols-1 gap-4 lg:grid-cols-2">
                    <FormInput placeholder="Default: 22" label="SSH Port" type="number" id="remoteServerPort" error={errors.remoteServerPort?.[0]} bind:value={remoteServerPort} />
                    <div>
                      <FormInput placeholder="Default: root" label="SSH User" id="remoteServerUser" error={errors.remoteServerUser?.[0]} bind:value={remoteServerUser} />
                    </div>
                  </div>
                {/if}
              </div>
              <button type="submit" class="button w-full lg:w-auto">Validate Connection</button>
            </form>
          {/snippet}
          {@render step('Server Configuration', 'Provide connection details for your remote server.', createServerActions)}
        {:else if currentState === 'validate-server'}
          {@render progressBar(2)}
          {#snippet validateActions()}
            <div class="flex w-full flex-col gap-4">
              <div class="overflow-hidden rounded-[10px] border border-neutral-200 dark:border-white/[0.08]">
                <div class="border-b border-neutral-200 px-4 py-2.5 dark:border-white/[0.08]">
                  <p class="text-[10px] font-semibold uppercase tracking-[0.08em] text-neutral-400 dark:text-fg-faint">Validation checkpoints</p>
                </div>
                <div class="divide-y divide-neutral-200 dark:divide-white/[0.07]">
                  <CheckpointItem icon="keys" title="Test SSH connection" description="Verify key-based authentication" status="idle" />
                  <CheckpointItem icon="servers" title="Check OS compatibility" description="Verify supported Linux distribution" status="idle" />
                  <CheckpointItem icon="layers" title="Install Docker Engine" description="Auto-install if version {minDockerVersion}+ not found" status="idle" />
                  <CheckpointItem icon="globe" title="Configure network" description="Set up Docker networks and proxy" status="idle" />
                </div>
              </div>
              <button type="button" class="button button-highlighted w-full justify-center" onclick={startValidation}>Start validation</button>
            </div>
          {/snippet}
          {#snippet validateDetails()}
            <p>{@render high('Automated Setup:')} coolify-rs installs Docker Engine, Docker Compose, and configures system requirements automatically.</p>
            <p>
              {@render high('Version Requirements:')} Minimum Docker Engine {minDockerVersion}.x required.
              <a target="_blank" class="underline hover:text-coollabs" href="https://docs.docker.com/engine/install/#server">Manual installation guide</a>
            </p>
            <p>{@render high('System Configuration:')} Sets up Docker networks, proxy configuration, and resource monitoring.</p>
          {/snippet}
          {@render step('Server Validation', `coolify-rs will automatically install Docker ${minDockerVersion}+ if not present.`, validateActions, validateDetails)}
        {:else if currentState === 'create-project'}
          {@render progressBar(3)}
          {#snippet projectActions()}
            <div class="w-full space-y-4">
              <button type="button" class="button button-highlighted w-full justify-center" onclick={() => run('createNewProject')}>Create "My First Project"</button>
              {#if projects.length > 0}
                <div class="relative">
                  <div class="absolute inset-0 flex items-center">
                    <div class="w-full border-t border-neutral-300 dark:border-coolgray-400"></div>
                  </div>
                  <div class="relative flex justify-center text-sm">
                    <span class="px-2 text-neutral-500 dark:text-neutral-400">Or use existing</span>
                  </div>
                </div>
                <form
                  class="flex flex-col gap-3 sm:flex-row sm:items-end"
                  onsubmit={(e) => {
                    e.preventDefault();
                    run('selectExistingProject', { id: selectedProject });
                  }}
                >
                  <div class="min-w-0 flex-1">
                    <Listbox id="selectedProject" label="Existing project" options={projects.map((p) => ({ value: String(p.value), label: p.label }))} value={String(selectedProject ?? '')} onchange={(v) => (selectedProject = Number(v))} />
                  </div>
                  <button type="submit" class="button">Use selected project</button>
                </form>
              {/if}
            </div>
          {/snippet}
          {@render step('Project Setup', projects.length > 0 ? 'You have existing projects. Select one or create a new project to organize your resources.' : 'Create your first project to organize applications, databases, and services.', projectActions)}
        {:else if currentState === 'create-resource'}
          {@render progressBar(3)}
          <div class="w-full max-w-3xl">
            <div class="mb-6 text-center">
              <div class="mx-auto mb-4 flex size-12 items-center justify-center rounded-[10px] border border-emerald-500/25 bg-emerald-500/10 text-emerald-600 dark:text-emerald-400">
                <Reicon name="check-circle" class="size-6" />
              </div>
              <h1 class="text-2xl! font-semibold!">Setup complete</h1>
              <p class="mt-1 text-[13px] text-neutral-500 dark:text-fg-dim">Your server is connected and ready. Start deploying your first resource.</p>
            </div>
            <div class="overflow-hidden rounded-[10px] border border-neutral-200 dark:border-white/[0.08]">
              <div class="border-b border-neutral-200 px-4 py-2.5 dark:border-white/[0.08]">
                <p class="text-[10px] font-semibold uppercase tracking-[0.08em] text-neutral-400 dark:text-fg-faint">What's configured</p>
              </div>
              <div class="divide-y divide-neutral-200 dark:divide-white/[0.07]">
                <CheckpointItem status="success" title="Server: {createdServer?.name}" description={createdServer?.ip} />
                <CheckpointItem status="success" title="Project: {createdProject?.name}" description="Production environment ready" />
                <CheckpointItem status="success" title="Docker Engine" description="Installed and running" />
              </div>
            </div>
            <div class="mt-5 flex flex-col items-center gap-4">
              <button type="button" class="button button-highlighted justify-center px-6" onclick={() => run('showNewResource', { serverId: createdServer?.id, projectId: createdProject?.id })}>Deploy your first resource</button>
              <div class={pillGroup}>
                <button type="button" class={pillButton} onclick={skipBoarding}>
                  <Reicon name="arrow-right" class="size-3.5 shrink-0" />
                  Go to dashboard
                </button>
                {@render help()}
              </div>
            </div>
          </div>
        {/if}
      </div>

      {#if currentState !== 'welcome' && currentState !== 'create-resource'}
        <div class="mx-auto mt-6 flex w-full max-w-3xl flex-col items-center gap-3">
          <div class={pillGroup}>
            <button type="button" class={pillButton} onclick={skipBoarding}>
              <Reicon name="arrow-right" class="size-3.5 shrink-0" />
              Skip setup
            </button>
            <button type="button" class={pillButton} onclick={restart}>
              <Reicon name="restart" class="size-3.5 shrink-0" />
              Restart
            </button>
            {@render help()}
          </div>
        </div>
      {/if}
    </section>
  {/if}
</main>

{#if validating}
  <ProcessDialog title="Server validation" closeWithX size="xl" onclose={() => (validating = false)}>
    <ValidateServer {progress} canRetry onretry={startValidation} onclose={() => (validating = false)} />
  </ProcessDialog>
{/if}

{#if helpOpen}
  <HelpForm title="Need Help?" onclose={() => (helpOpen = false)} />
{/if}

{#if provision}
  <Modal title="Connect a {providerTitle[provision]} Server" onclose={() => (provision = null)}>
    {#if provision === 'hetzner'}
      <ServerNewHetzner />
    {:else if provision === 'vultr'}
      <ServerNewVultr />
    {:else if provision === 'digital-ocean'}
      <ServerNewDigitalOcean />
    {:else}
      <ServerNewHostinger />
    {/if}
  </Modal>
{/if}
