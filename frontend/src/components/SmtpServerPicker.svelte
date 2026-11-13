<script lang="ts">
  // Port of upstream livewire/partials/smtp-server-picker.blade.php (App\Livewire\Concerns\ManagesSmtpServers):
  // the named SMTP servers and the fields of the server in use, in groups. The Add server modal
  // also offers the SMTP templates. Used on the transactional email and email notification
  // pages; put it inside a grid with three columns.
  import Callout from './Callout.svelte';
  import ConfirmModal from './ConfirmModal.svelte';
  import FormInput from './FormInput.svelte';
  import Listbox from './Listbox.svelte';
  import Modal from './Modal.svelte';
  import Reicon from './Reicon.svelte';
  import type { SmtpFieldKeys, SmtpPreset, SmtpServerOption } from '../lib/smtpServers';

  let {
    form = $bindable(),
    keys,
    canUpdate,
    isNew,
    activeName,
    options,
    presets,
    onswitch,
    onrename,
    ondelete,
    onadd,
    oncancel,
  }: {
    /** The page's form: the fields of the server in use are read and written through `keys`. */
    form: Record<string, any>;
    keys: SmtpFieldKeys;
    /** `auth()->user()->can('update', $settings)`. */
    canUpdate: boolean;
    /** `$isNewSmtpServer`. */
    isNew: boolean;
    /** `$settings->smtpServerName()`: the saved name of the server in use. */
    activeName: string;
    /** `smtpServerOptions()`. */
    options: SmtpServerOption[];
    presets: SmtpPreset[];
    onswitch: (id: string) => Promise<void> | void;
    onrename: (name: string) => Promise<void> | void;
    ondelete: () => Promise<void> | void;
    onadd: (name: string, template: string) => Promise<void> | void;
    oncancel: () => void;
  } = $props();

  let selected = $state('active');
  let renameOpen = $state(false);
  let renamed = $state('');
  let deleteOpen = $state(false);
  let addOpen = $state(false);
  let newName = $state('');
  let newTemplate = $state('');

  // Same rule as SmtpPresets::requiresLogin(), so the required marks follow the fields while you type.
  const providerHosts = $derived(presets.map((p) => p.host));
  const isProviderHost = (host: unknown) => providerHosts.includes(String(host ?? '').trim().toLowerCase());

  const groupClass = 'flex flex-col gap-3 border-t border-neutral-200 pt-4 lg:col-span-3 dark:border-white/[0.08]';
  const groupTitleClass = 'text-sm font-semibold text-black dark:text-fg';

  async function switchServer(id: string) {
    try {
      await onswitch(id);
    } finally {
      selected = 'active';
    }
  }

  function rename(e: SubmitEvent) {
    e.preventDefault();
    renameOpen = false;
    onrename(renamed);
  }

  function add(e: SubmitEvent) {
    e.preventDefault();
    addOpen = false;
    onadd(newName, newTemplate);
    newName = '';
    newTemplate = '';
  }
</script>

<div class="flex flex-col gap-3 lg:col-span-3">
  {#if isNew}
    <Callout type="info" title="New SMTP server: {form[keys.name] ?? ''}">
      Enter the settings, then save. The server
      {activeName} stays in the server list.
      <div class="mt-2">
        <button type="button" class="button" onclick={oncancel}>Cancel</button>
      </div>
    </Callout>
  {:else}
    <div class="flex flex-wrap items-end gap-2">
      <div class="w-full sm:w-72">
        <Listbox id="selectedSmtpServer" label="SMTP server" disabled={!canUpdate} bind:value={selected} onchange={switchServer} {options} />
      </div>
      {#if canUpdate}
        <button
          type="button"
          class="icon-button mb-0.5"
          title="Rename server"
          aria-label="Rename SMTP server {activeName}"
          onclick={() => {
            renamed = activeName;
            renameOpen = true;
          }}
        >
          <Reicon name="edit" class="size-3.5" />
        </button>
        {#if options.length > 1}
          <button
            type="button"
            class="icon-button mb-0.5 text-red-500 hover:text-red-600 dark:text-red-400 dark:hover:text-red-300"
            title="Delete server"
            aria-label="Delete SMTP server {activeName}"
            onclick={() => (deleteOpen = true)}
          >
            <Reicon name="trash" class="size-3.5" />
          </button>
        {/if}
        <button type="button" class="button ml-2" onclick={() => (addOpen = true)}>
          <Reicon name="plus" class="size-3.5" />
          Add server
        </button>
      {/if}
    </div>
  {/if}
</div>

<div class={groupClass}>
  <h4 class={groupTitleClass}>Sender</h4>
  <div class="grid gap-4 lg:grid-cols-2">
    <FormInput disabled={!canUpdate} required id="smtpFromName" helper="Name shown in email from this server." label="From name" bind:value={form[keys.fromName]} />
    <FormInput
      disabled={!canUpdate}
      required
      id="smtpFromAddress"
      helper="Address used in email from this server. It must be on a domain that this server can send from."
      label="From address"
      bind:value={form[keys.fromAddress]}
    />
  </div>
</div>

<div class={groupClass}>
  <h4 class={groupTitleClass}>Connection</h4>
  <div class="grid gap-4 lg:grid-cols-3">
    <FormInput disabled={!canUpdate} required id="smtpHost" placeholder="smtp.mailgun.org" label="Host" bind:value={form[keys.host]} />
    <FormInput disabled={!canUpdate} required id="smtpPort" type="number" placeholder="587" label="Port" bind:value={form[keys.port]} />
    <Listbox
      required
      id="smtpEncryption"
      label="Encryption"
      disabled={!canUpdate}
      bind:value={form[keys.encryption]}
      options={[
        { value: 'starttls', label: 'StartTLS' },
        { value: 'tls', label: 'TLS / SSL' },
        { value: 'none', label: 'None' },
      ]}
    />
  </div>
</div>

<div class={groupClass}>
  <h4 class={groupTitleClass}>Authentication</h4>
  <div class="grid gap-4 lg:grid-cols-2">
    <FormInput
      disabled={!canUpdate}
      id="smtpUsername"
      label="Username"
      required={isProviderHost(form[keys.host])}
      helper="The SMTP username from your provider. Leave it empty only for a relay that does not need a login, for example a local mail server."
      bind:value={form[keys.username]}
    />
    {#if canUpdate}
      <FormInput
        id="smtpPassword"
        type="password"
        label="Password"
        autocomplete="new-password"
        required={isProviderHost(form[keys.host]) || !!form[keys.username]}
        helper="The SMTP password from your provider. It is required when you enter a username."
        bind:value={form[keys.password]}
      />
    {:else}
      <FormInput id="smtpPassword" disabled label="Password" value="Hidden (only admins can view)" />
    {/if}
  </div>
</div>

<div class={groupClass}>
  <h4 class={groupTitleClass}>Advanced</h4>
  <div class="grid gap-4 lg:grid-cols-2">
    <FormInput disabled={!canUpdate} id="smtpTimeout" type="number" helper="Maximum delivery time in seconds." label="Timeout" bind:value={form[keys.timeout]} />
    <FormInput
      disabled={!canUpdate}
      id="smtpEhloDomain"
      placeholder="coolify.example.com"
      helper="Fully qualified domain sent in the SMTP EHLO command. Uses the system default when empty."
      label="EHLO domain"
      bind:value={form[keys.ehlo]}
    />
  </div>
</div>

{#if renameOpen}
  <Modal title="Rename SMTP server" onclose={() => (renameOpen = false)}>
    <form onsubmit={rename} class="application-settings-form flex flex-col gap-4">
      <FormInput id="renamedSmtpServerName" label="Name" required helper="Name to find this server in the SMTP server list." bind:value={renamed} />
      <div class="flex justify-end border-t border-neutral-200 pt-4 dark:border-white/[0.08]">
        <button type="submit" class="button button-highlighted">Save</button>
      </div>
    </form>
  </Modal>
{/if}

{#if deleteOpen}
  <ConfirmModal
    title="Delete SMTP server?"
    actions={[`The SMTP server ${activeName} is deleted. Coolify then uses the next server in the list.`]}
    confirmWithText={false}
    confirmWithPassword={false}
    step2ButtonText="Delete server"
    onconfirm={async () => {
      await ondelete();
    }}
    onclose={() => (deleteOpen = false)}
  />
{/if}

{#if addOpen}
  <Modal title="Add SMTP server" onclose={() => (addOpen = false)}>
    <form onsubmit={add} class="application-settings-form flex flex-col gap-4">
      <FormInput
        id="newSmtpServerName"
        label="Name"
        placeholder="PostShiba production"
        helper="Name to find this server in the SMTP server list."
        required
        bind:value={newName}
      />
      <Listbox
        id="newSmtpServerTemplate"
        label="Template"
        placeholder="None (enter the settings yourself)"
        helper="Fills in the host, port, and encryption of the provider. You then enter the username and password from the provider."
        bind:value={newTemplate}
        options={presets.map((p) => ({ value: p.value, label: p.label }))}
      />
      <div class="flex justify-end border-t border-neutral-200 pt-4 dark:border-white/[0.08]">
        <button type="submit" class="button button-highlighted">Add server</button>
      </div>
    </form>
  </Modal>
{/if}
