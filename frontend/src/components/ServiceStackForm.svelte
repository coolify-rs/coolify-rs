<script lang="ts">
  // Port of upstream livewire/project/service/stack-form.blade.php (StackForm)
  // and livewire/project/service/edit-compose.blade.php (EditCompose).
  import { untrack } from 'svelte';
  import Callout from './Callout.svelte';
  import Checkbox from './Checkbox.svelte';
  import FormInput from './FormInput.svelte';
  import FormTextarea from './FormTextarea.svelte';
  import Helper from './Helper.svelte';
  import Listbox from './Listbox.svelte';
  import Modal from './Modal.svelte';
  import MonacoEditor from './MonacoEditor.svelte';
  import Reicon from './Reicon.svelte';
  import ServiceSharedPlaceholder from './ServiceSharedPlaceholder.svelte';
  import SettingsSection from './SettingsSection.svelte';
  import SettingsSubsection from './SettingsSubsection.svelte';
  import UnsavedBar from './UnsavedBar.svelte';
  import { ApiError } from '../lib/api';
  import { servicesApi, type ServicePage } from '../lib/servicesApi';
  import { notify } from '../lib/toast.svelte';

  let { data, onsaved }: { data: ServicePage; onsaved: () => Promise<void> } = $props();

  const uuid = $derived(data.service.uuid);
  const canUpdate = $derived(data.can_update);

  type Form = { name: string; description: string; fields: Record<string, string> };
  function snapshot(): Form {
    return {
      name: data.service.name,
      description: data.service.description ?? '',
      fields: Object.fromEntries(data.fields.filter((f) => !f.isLocked).map((f) => [f.key, f.value ?? ''])),
    };
  }
  let form = $state<Form>(untrack(snapshot));
  let saved = $state<Form>(untrack(snapshot));
  let connect = $state(untrack(() => data.service.connect_to_docker_network));
  let saving = $state(false);
  let error = $state<ApiError | null>(null);

  const dirty = $derived(JSON.stringify(form) !== JSON.stringify(saved));

  let composeOpen = $state(false);
  let detailsOpen = $state(false);
  let raw = $state(untrack(() => data.service.docker_compose_raw ?? ''));
  let preview = $state(false);
  let validating = $state(false);
  let composeSaving = $state(false);
  let plainEditor = $state(false);
  let labelEscape = $state(untrack(() => data.service.is_container_label_escape_enabled));

  async function submit(compose?: string) {
    saving = true;
    error = null;
    try {
      const r = await servicesApi.submit(uuid, {
        name: form.name,
        description: form.description,
        connect_to_docker_network: connect,
        fields: form.fields,
        ...(compose !== undefined ? { docker_compose_raw: compose } : {}),
      });
      notify.success(r.message);
      await onsaved();
      saved = snapshot();
      form = snapshot();
      return true;
    } catch (e) {
      error = e instanceof ApiError ? e : new ApiError(0, String(e));
      if (!Object.keys(error.errors).length) notify.error(error.message);
      return false;
    } finally {
      saving = false;
    }
  }

  function reset() {
    form = JSON.parse(JSON.stringify(saved));
    error = null;
  }

  async function saveNetwork(v: boolean) {
    connect = v;
    try {
      notify.success((await servicesApi.saveNetwork(uuid, v)).message);
    } catch (e) {
      notify.error(e instanceof ApiError ? e.message : String(e));
    }
  }

  async function validateCompose() {
    validating = true;
    try {
      notify.success((await servicesApi.validateCompose(uuid, raw)).message);
    } catch (e) {
      notify.error(e instanceof ApiError ? e.message : String(e));
    } finally {
      validating = false;
    }
  }

  async function saveCompose() {
    composeSaving = true;
    if (await submit(raw)) composeOpen = false;
    composeSaving = false;
  }

  async function saveEscape(v: boolean) {
    try {
      notify.success((await servicesApi.saveLabelEscape(uuid, v)).message);
    } catch (e) {
      notify.error(e instanceof ApiError ? e.message : String(e));
    }
  }
</script>

<form
  class="application-settings-form flex flex-col gap-6"
  onsubmit={(e) => {
    e.preventDefault();
    submit();
  }}
>
  <UnsavedBar {dirty} {saving} onsave={() => submit()} onreset={reset} />

  <SettingsSection title="Service details" description="Manage the identity, Compose configuration, and network of this service.">
    {#snippet actions()}
      <div class="flex items-center gap-2">
        {#if canUpdate}
          <button
            type="button"
            class="button"
            onclick={() => {
              raw = data.service.docker_compose_raw ?? '';
              preview = false;
              composeOpen = true;
            }}>Edit Compose file</button
          >
        {/if}
        <button type="button" class="button" onclick={() => (detailsOpen = true)}>Details</button>
      </div>
    {/snippet}

    <div class="grid gap-4 lg:grid-cols-2">
      <FormInput id="name" label="Service name" required placeholder="My WordPress site" disabled={!canUpdate} bind:value={form.name} error={error?.field('name')} />
      <FormInput id="description" label="Description" disabled={!canUpdate} bind:value={form.description} error={error?.field('description')} />
    </div>
    <SettingsSubsection id="service-network-section" title="Network" helper="Control whether this Compose stack joins Coolify's predefined network.">
      <Listbox
        id="connectToDockerNetwork"
        label="Network attachment"
        disabled={!canUpdate}
        value={connect}
        options={[
          { value: false, label: 'Use the stack network only' },
          { value: true, label: 'Connect to the predefined Coolify network' },
        ]}
        onchange={saveNetwork}
      />
    </SettingsSubsection>
  </SettingsSection>

  {#if data.fields.length > 0}
    <SettingsSection title="Service configuration" description="Template-specific values exposed by this service.">
      <div class="grid gap-4 lg:grid-cols-2">
        {#each data.fields as field (field.key)}
          <div>
            <div class="mb-1.5 flex items-center gap-1.5 text-[12px] font-medium">
              <span>
                {#if field.serviceName}{field.serviceName} ·{/if}
                {field.name}
              </span>
              <Helper helper={field.customHelper || `Variable name: ${field.key}`} />
            </div>
            {#if field.isLocked}
              <FormInput id="fields.{field.key}.locked" disabled value="Locked (shown once)" />
            {:else if data.is_password_hidden && field.isPassword}
              <FormInput id="fields.{field.key}.hidden" disabled value="Hidden (only admins can view)" />
            {:else}
              <FormInput
                id="fields.{field.key}.value"
                type={field.isPassword ? 'password' : 'text'}
                required={field.rules.includes('required')}
                disabled={!canUpdate}
                bind:value={form.fields[field.key]}
                error={error?.field(`fields.${field.key}.value`)}
              />
            {/if}
          </div>
        {/each}
      </div>
    </SettingsSection>
  {/if}
</form>

{#if composeOpen}
  <Modal title="Docker Compose" isLarge onclose={() => (composeOpen = false)}>
    <div class="flex min-h-0 flex-col gap-3">
      <Callout type="info" title="Volume names">Volume names are prefixed with the service UUID when you save to prevent collisions.</Callout>

      <div
        class="compose-editor-container min-h-[24rem] overflow-hidden rounded-lg border border-neutral-200 bg-white dark:border-white/[0.10] dark:bg-[#0b0b0c]"
        style="--editor-height: clamp(24rem, calc(100dvh - 25rem), 48rem)"
      >
        {#if preview}
          <div class="font-mono">
            <FormTextarea id="dockerCompose" monospace readonly rows={20} value={data.service.docker_compose ?? ''} />
          </div>
        {:else if plainEditor}
          <div class="font-mono">
            <FormTextarea id="dockerComposeRaw" monospace rows={20} bind:value={raw} />
          </div>
        {:else}
          <div class="font-mono"><MonacoEditor bind:value={raw} language="yaml" /></div>
        {/if}
      </div>

      <div class="flex flex-col items-stretch gap-2 rounded-lg border border-neutral-200 bg-neutral-50 p-1 sm:flex-row sm:flex-wrap sm:items-center dark:border-white/[0.08] dark:bg-white/[0.05]">
        <Checkbox
          id="isContainerLabelEscapeEnabled"
          label="Escape special characters in labels"
          helper="By default, $ (and other characters) is escaped. A $ in a label is saved as $$. Turn this off to use environment variables inside labels."
          bind:checked={labelEscape}
          onchange={saveEscape}
        />
        <Checkbox id="showNormalTextarea" label="Use plain-text editor" bind:checked={plainEditor} />
      </div>
    </div>
    {#snippet footer()}
      <div class="flex flex-wrap items-center justify-end gap-2">
        <button type="button" class="button" onclick={() => (preview = !preview)}>
          <Reicon name="eye" class="size-3.5" />
          <span>{preview ? 'Back to source Compose' : 'Preview generated Compose'}</span>
        </button>
        {#if !data.service.service_type}
          <button type="button" class="button" disabled={validating} onclick={validateCompose}>Validate</button>
        {/if}
        <button type="button" class="button button-highlighted" disabled={composeSaving} onclick={saveCompose}>Save changes</button>
      </div>
    {/snippet}
  </Modal>
{/if}

{#if detailsOpen}
  <Modal title="Resource details" onclose={() => (detailsOpen = false)}>
    <ServiceSharedPlaceholder section="Project/Shared/ResourceDetails" />
  </Modal>
{/if}
