<script lang="ts">
  // Port of upstream livewire/notifications/discord.blade.php,
  // livewire/notifications/slack.blade.php, livewire/notifications/telegram.blade.php,
  // livewire/notifications/pushover.blade.php and livewire/notifications/webhook.blade.php.
  import FormInput from '../components/FormInput.svelte';
  import Listbox from '../components/Listbox.svelte';
  import UnsavedBar from '../components/UnsavedBar.svelte';
  import SettingsSection from '../components/SettingsSection.svelte';
  import NotificationLayout from '../components/NotificationLayout.svelte';
  import ChannelActions from '../components/ChannelActions.svelte';
  import EventGrid from '../components/EventGrid.svelte';
  import { ApiError } from '../lib/api';
  import { notificationsUi, type ChannelData, type Values } from '../lib/notificationsApi';
  import { notify } from '../lib/toast.svelte';

  interface Field {
    column: string;
    label: string;
    helper: string;
    newPassword?: boolean;
  }
  interface Config {
    title: string;
    heading: string;
    description: string;
    fields: Field[];
    single: boolean;
  }

  const configs: Record<string, Config> = {
    discord: {
      title: 'Discord Notifications',
      heading: 'Discord',
      description: 'Send team notifications to a Discord channel through an incoming webhook.',
      single: true,
      fields: [{ column: 'discord_webhook_url', label: 'Webhook URL', helper: 'Create an incoming webhook in your Discord server settings.' }],
    },
    slack: {
      title: 'Slack Notifications',
      heading: 'Slack',
      description: 'Send team notifications to Slack through an incoming webhook.',
      single: true,
      fields: [{ column: 'slack_webhook_url', label: 'Webhook URL', helper: 'Create an incoming webhook in your Slack app settings.' }],
    },
    webhook: {
      title: 'Webhook Notifications',
      heading: 'Webhook',
      description: 'Send JSON event payloads to your own HTTP endpoint.',
      single: true,
      fields: [{ column: 'webhook_url', label: 'Webhook URL', helper: 'coolify-rs sends POST requests to this HTTP or HTTPS endpoint.' }],
    },
    telegram: {
      title: 'Telegram Notifications',
      heading: 'Telegram',
      description: 'Deliver team notifications through a Telegram bot and chat.',
      single: false,
      fields: [
        { column: 'telegram_token', label: 'Bot API token', helper: 'Create a bot with BotFather to obtain this token.', newPassword: true },
        { column: 'telegram_chat_id', label: 'Chat ID', helper: 'Add the bot to your chat, then enter that chat ID.', newPassword: true },
      ],
    },
    pushover: {
      title: 'Pushover Notifications',
      heading: 'Pushover',
      description: 'Deliver team alerts through your Pushover application.',
      single: false,
      fields: [
        { column: 'pushover_user_key', label: 'User key', helper: 'Find this in the Pushover dashboard.' },
        { column: 'pushover_api_token', label: 'API token', helper: 'Create an application in Pushover to generate this token.' },
      ],
    },
  };

  let { channel }: { channel: string } = $props();
  const config = $derived(configs[channel]);
  $effect(() => {
    document.title = `${config.title} | coolify-rs`;
  });

  let data = $state<ChannelData | null>(null);
  let form = $state<Values>({});
  let saved = $state<Values>({});
  let saving = $state(false);

  function apply(d: ChannelData) {
    data = d;
    saved = { ...d.settings };
    form = { ...d.settings };
  }
  notificationsUi.get(channel).then(apply);

  const dirty = $derived(JSON.stringify(form) !== JSON.stringify(saved));
  const canUpdate = $derived(!!data?.can_update);
  const enabledColumn = $derived(`${channel}_enabled`);

  function failed(e: unknown) {
    const err = e instanceof ApiError ? e : new ApiError(0, String(e));
    if (err.details) notify.error(err.message, err.details);
    else notify.failure(err.message);
  }

  /** `saveModel()`: the whole form is saved. `revert` undoes the property on failure. */
  async function run(action: string, revert?: () => void) {
    saving = true;
    try {
      const d = await notificationsUi.action(channel, action, $state.snapshot(form));
      apply(d);
      notify.success(d.message ?? 'Settings saved.');
    } catch (e) {
      revert?.();
      failed(e);
    } finally {
      saving = false;
    }
  }

  function toggleEnabled(next: boolean) {
    const before = !next;
    form[enabledColumn] = next;
    run('toggle', () => (form[enabledColumn] = before));
  }

  /** `Notifications\Concerns\TogglesNotificationEvents::toggleEvent`. */
  function toggleEvent(column: string) {
    form[column] = !form[column];
    run('save', () => (form[column] = !form[column]));
  }

  async function sendTest() {
    try {
      const r = await notificationsUi.test(channel);
      notify.success(r.message);
    } catch (e) {
      failed(e);
    }
  }
</script>

{#key channel}
  <div>
    <NotificationLayout>
      {#if data}
        <div class="application-settings-form flex flex-col gap-6">
          <form
            onsubmit={(e) => {
              e.preventDefault();
              run('save');
            }}
          >
            <UnsavedBar {dirty} {saving} onsave={() => run('save')} onreset={() => (form = { ...saved })} />
            <SettingsSection title={config.heading} description={config.description}>
              {#snippet actions()}
                <ChannelActions enabled={!!form[enabledColumn]} {canUpdate} ontoggle={toggleEnabled} ontest={sendTest} />
              {/snippet}
              <div class="grid gap-4 lg:grid-cols-2">
                {#if channel === 'discord'}
                  <Listbox
                    id="discord_ping_enabled"
                    label="Critical event mention"
                    helper="Mention @here when a critical event occurs."
                    disabled={!canUpdate}
                    bind:value={form.discord_ping_enabled}
                    onchange={(v) => run('save', () => (form.discord_ping_enabled = !v))}
                    options={[
                      { value: true, label: 'Mention @here' },
                      { value: false, label: 'Do not mention' },
                    ]}
                  />
                {/if}
                {#each config.fields as f (f.column)}
                  <div class={config.single ? 'lg:col-span-2' : 'contents'}>
                    {#if canUpdate}
                      <FormInput
                        type="password"
                        id={f.column}
                        label={f.label}
                        helper={f.helper}
                        required={!!form[enabledColumn]}
                        autocomplete={f.newPassword ? 'new-password' : undefined}
                        bind:value={form[f.column]}
                      />
                    {:else}
                      <FormInput id={f.column} disabled label={f.label} value="Hidden (only admins can view)" />
                    {/if}
                  </div>
                {/each}
              </div>
            </SettingsSection>
          </form>

          <EventGrid {channel} settings={saved} bind:form threaded={channel === 'telegram'} disabled={!canUpdate} ontoggle={toggleEvent} />
        </div>
      {/if}
    </NotificationLayout>
  </div>
{/key}
