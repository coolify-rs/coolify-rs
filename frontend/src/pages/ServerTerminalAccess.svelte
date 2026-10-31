<script lang="ts">
  // Port of upstream livewire/server/security/terminal-access.blade.php.
  import Reicon from '../components/Reicon.svelte';
  import ConfirmModal from '../components/ConfirmModal.svelte';
  import ServerFrame from '../components/ServerFrame.svelte';
  import SettingsSection from '../components/SettingsSection.svelte';
  import { ApiError } from '../lib/api';
  import { get, post } from '../lib/serverApi';
  import { session } from '../lib/session.svelte';
  import { notify } from '../lib/toast.svelte';

  let { uuid }: { uuid: string } = $props();
  type Data = { is_terminal_enabled: boolean; confirm: { text: boolean; password: boolean } };
  let data = $state<Data | null>(null);
  let confirming = $state(false);
  let serverName = $state('');
  const url = $derived(`/server-page/${encodeURIComponent(uuid)}/terminal-access`);

  async function load() {
    data = await get<Data>(url);
  }
  async function toggle(password: string): Promise<string | void> {
    try {
      const r = await post<{ message: string; is_terminal_enabled: boolean }>(url, { password });
      if (data) data.is_terminal_enabled = r.is_terminal_enabled;
      notify.success(r.message);
    } catch (e) {
      if (e instanceof ApiError && e.message === 'The provided password is incorrect.') return e.message;
      throw e;
    }
  }
</script>

<ServerFrame {uuid} active="security" sub="terminal-access" title="Terminal Access" onloaded={(f) => ((serverName = f.name), load())}>
  {#snippet children()}
    {#if data}
      <div class="application-settings-form w-full">
        <SettingsSection id="server-terminal-access-section" title="Terminal access" description="Control dashboard terminal access for this server and its containers.">
          <div class="flex flex-col justify-between gap-4 sm:flex-row sm:items-center">
            <div class="flex items-start gap-3">
              <div class="flex size-9 shrink-0 items-center justify-center rounded-lg bg-neutral-100 text-neutral-500 dark:bg-white/[0.06] dark:text-fg-dim">
                <Reicon name="browser-terminal" class="size-4" />
              </div>
              <div>
                <p class="text-sm font-medium text-neutral-950 dark:text-fg">{data.is_terminal_enabled ? 'Dashboard terminal is available' : 'Dashboard terminal is blocked'}</p>
                <p class="mt-1 max-w-xl text-xs leading-5 text-neutral-500 dark:text-fg-dim">
                  This setting applies to every user, including administrators and the team owner. Only administrators and owners can change it.
                </p>
              </div>
            </div>
            {#if session.isAdmin}
              <div>
                <button type="button" class="button {data.is_terminal_enabled ? '' : 'button-highlighted'}" onclick={() => (confirming = true)}>
                  {data.is_terminal_enabled ? 'Disable terminal' : 'Enable terminal'}
                </button>
              </div>
            {/if}
          </div>
        </SettingsSection>
      </div>
    {/if}
  {/snippet}
</ServerFrame>

{#if confirming && data}
  <ConfirmModal
    title="Confirm Terminal Access Change?"
    actions={[
      data.is_terminal_enabled ? 'Disable terminal access for this server and all of its containers.' : 'Enable terminal access for this server and all of its containers.',
      'The change takes effect immediately for every user.',
    ]}
    confirmWithText={data.confirm.text}
    confirmWithPassword={data.confirm.password}
    confirmationText={serverName}
    shortConfirmationLabel="Server Name"
    step3ButtonText={data.is_terminal_enabled ? 'Disable Terminal' : 'Enable Terminal'}
    onconfirm={toggle}
    onclose={() => (confirming = false)}
  />
{/if}
