<script lang="ts">
  // coolify-rs's own (ADR 0014): a plugin's settings for the team, in the modal-input dialog with the form
  // parts of Coolify's settings pages. Owners and admins edit; members see the values that are not secret.
  // A secret is never sent back to the browser: its field is empty with a "set" placeholder, an empty field
  // keeps it, and Clear removes it.
  import Modal from './Modal.svelte';
  import FormInput from './FormInput.svelte';
  import Listbox from './Listbox.svelte';
  import Callout from './Callout.svelte';
  import { ApiError } from '../lib/api';
  import { notify } from '../lib/toast.svelte';
  import { pluginsUi, type PluginSetting } from '../lib/plugins.svelte';

  let {
    plugin,
    canManage,
    onclose,
    onsaved,
  }: {
    plugin: { id: string; name: string; settings: PluginSetting[] };
    canManage: boolean;
    onclose: () => void;
    onsaved: () => void;
  } = $props();

  /** The form's starting values: the team's values, and empty fields for secrets. */
  function initial(list: PluginSetting[]): Record<string, string> {
    return Object.fromEntries(list.map((s) => [s.key, s.secret ? '' : (s.value ?? '')]));
  }

  // A snapshot when the dialog opens; each save answers the settings as they are afterwards.
  // svelte-ignore state_referenced_locally
  let settings = $state<PluginSetting[]>(plugin.settings);
  // svelte-ignore state_referenced_locally
  let values = $state<Record<string, string>>(initial(plugin.settings));
  let busy = $state(false);
  let error = $state<ApiError | null>(null);

  function reset(list: PluginSetting[]) {
    settings = list;
    values = initial(list);
  }

  const fieldId = (s: PluginSetting) => `plugin-setting-${s.key}`;

  function secretPlaceholder(s: PluginSetting): string {
    if (s.set) return '••••• (set)';
    return canManage ? '' : 'Not set';
  }

  function selectOptions(s: PluginSetting) {
    const options = s.options.map((o) => ({ value: o, label: o }));
    if (s.required) return options;
    return [{ value: '', label: s.default ? `Default (${s.default})` : 'Not set' }, ...options];
  }

  /** What changed: values that are not secret when they differ, secrets only when something was typed. */
  function changes(): Record<string, string | null> {
    const out: Record<string, string | null> = {};
    for (const s of settings) {
      const v = (values[s.key] ?? '').trim();
      if (s.secret) {
        if (v) out[s.key] = v;
      } else if (v !== (s.value ?? '')) {
        out[s.key] = v || null;
      }
    }
    return out;
  }

  async function send(body: Record<string, string | null>, message: string): Promise<PluginSetting[] | null> {
    busy = true;
    error = null;
    try {
      const r = await pluginsUi.updateTeamSettings(plugin.id, body);
      notify.success(message);
      onsaved();
      return r.settings;
    } catch (e) {
      if (e instanceof ApiError) error = e;
      notify.failure(e instanceof ApiError ? e.message : String(e));
      return null;
    } finally {
      busy = false;
    }
  }

  async function submit(e: SubmitEvent) {
    e.preventDefault();
    if (busy || !canManage) return;
    const body = changes();
    if (!Object.keys(body).length) return onclose();
    const saved = await send(body, `${plugin.name} settings saved.`);
    if (saved) {
      reset(saved);
      onclose();
    }
  }

  /** Removes a secret at once; what was typed in the other fields stays in the form. */
  async function clear(s: PluginSetting) {
    if (busy) return;
    const saved = await send({ [s.key]: null }, `${s.label} cleared.`);
    if (saved) {
      settings = saved;
      values[s.key] = '';
    }
  }
</script>

<Modal title="{plugin.name} settings" subtitle={plugin.id} {onclose}>
  <form id="plugin-settings-form" class="application-settings-form flex w-full flex-col gap-4" onsubmit={submit}>
    {#if !canManage}
      <Callout type="info" title="Read only" class="mb-1">
        Only team owners and admins can change these settings. Secrets are never shown.
      </Callout>
    {/if}
    {#each settings as s (s.key)}
      <div data-setting={s.key}>
        {#if s.kind === 'select' && !s.secret}
          <Listbox
            id={fieldId(s)}
            label={s.label}
            helper={s.help ?? undefined}
            required={s.required}
            disabled={!canManage || busy}
            options={selectOptions(s)}
            placeholder={s.default ? `Default (${s.default})` : 'Not set'}
            bind:value={values[s.key]}
            error={error?.field(s.key)}
          />
        {:else if s.secret}
          <div class="flex items-end gap-2">
            <div class="min-w-0 flex-1">
              <FormInput
                id={fieldId(s)}
                type="password"
                label={s.label}
                helper={s.help ?? undefined}
                required={s.required && !s.set && canManage}
                copyable={false}
                peek={canManage}
                disabled={!canManage || busy}
                placeholder={secretPlaceholder(s)}
                bind:value={values[s.key]}
                error={error?.field(s.key)}
              />
            </div>
            {#if canManage && s.set}
              <button type="button" class="button" disabled={busy} onclick={() => clear(s)}>Clear</button>
            {/if}
          </div>
        {:else}
          <FormInput
            id={fieldId(s)}
            type={s.kind === 'url' ? 'url' : 'text'}
            label={s.label}
            helper={s.help ?? undefined}
            required={s.required && canManage}
            readonly={!canManage}
            disabled={busy}
            placeholder={s.default ?? (s.kind === 'url' ? 'https://' : '')}
            bind:value={values[s.key]}
            error={error?.field(s.key)}
          />
        {/if}
      </div>
    {/each}
  </form>
  {#snippet footer()}
    <button type="button" class="button" onclick={onclose}>{canManage ? 'Cancel' : 'Close'}</button>
    {#if canManage}
      <button type="submit" form="plugin-settings-form" class="button button-highlighted" disabled={busy}>Save</button>
    {/if}
  {/snippet}
</Modal>
