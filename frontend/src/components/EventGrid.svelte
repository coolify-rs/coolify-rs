<script lang="ts">
  // Port of upstream components/notification/event-grid.blade.php.
  import SettingsSection from './SettingsSection.svelte';
  import EventMultiselect from './EventMultiselect.svelte';
  import FormInput from './FormInput.svelte';
  import type { Values } from '../lib/notificationsApi';

  let {
    channel,
    settings,
    form = $bindable(),
    threaded = false,
    disabled = false,
    ontoggle,
  }: { channel: string; settings: Values; form: Values; threaded?: boolean; disabled?: boolean; ontoggle: (column: string) => void } = $props();

  const groups: [string, { key: string; label: string }[]][] = [
    [
      'Deployments',
      [
        { key: 'deployment_success', label: 'Deployment success' },
        { key: 'deployment_failure', label: 'Deployment failure' },
      ],
    ],
    [
      'Resources',
      [
        { key: 'status_change', label: 'Resource status changes' },
        { key: 'restart_limit_reached', label: 'Restart limit reached' },
        { key: 'image_vulnerabilities', label: 'Image vulnerabilities' },
      ],
    ],
    [
      'Backups',
      [
        { key: 'backup_success', label: 'Backup success' },
        { key: 'backup_failure', label: 'Backup failure' },
      ],
    ],
    [
      'Scheduled tasks',
      [
        { key: 'scheduled_task_success', label: 'Scheduled task success' },
        { key: 'scheduled_task_failure', label: 'Scheduled task failure' },
      ],
    ],
    [
      'Servers',
      [
        { key: 'docker_cleanup_success', label: 'Docker cleanup success' },
        { key: 'docker_cleanup_failure', label: 'Docker cleanup failure' },
        { key: 'server_disk_usage', label: 'Disk usage warning' },
        { key: 'server_reachable', label: 'Server reachable' },
        { key: 'server_unreachable', label: 'Server unreachable' },
        { key: 'server_patch', label: 'Server patching' },
        { key: 'traefik_outdated', label: 'Traefik proxy outdated' },
      ],
    ],
  ];

  const column = (key: string) => `${key}_${channel}_notifications`;
  const slug = (s: string) => s.toLowerCase().replace(/\s+/g, '-');

  const threadGroups = $derived(
    groups
      .map(([group, events]) => [group, events.filter((e) => settings[column(e.key)])] as const)
      .filter(([, events]) => events.length > 0),
  );
</script>

<div class="flex flex-col gap-6">
  <SettingsSection title="Notification events" description="Choose which events send a notification on this channel.">
    <div class="grid gap-4 lg:grid-cols-2">
      {#each groups as [group, events] (group)}
        <div class="min-w-0">
          <EventMultiselect
            id={slug(`${channel}-${group}-events`)}
            label={group}
            {disabled}
            {ontoggle}
            events={events.map((e) => ({ column: column(e.key), label: e.label, enabled: !!settings[column(e.key)] }))}
          />
        </div>
      {/each}
    </div>
  </SettingsSection>

  {#if threaded}
    <SettingsSection
      title="Forum topics"
      description="Optional. Route enabled events to a Telegram forum topic using its message thread ID. Leave blank to post in the main chat."
    >
      {#if threadGroups.length === 0}
        <p class="text-[13px] leading-relaxed text-neutral-500 dark:text-fg-dim">Enable one or more events above to assign forum topic IDs.</p>
      {:else}
        <div class="flex flex-col gap-5">
          {#each threadGroups as [group, events] (group)}
            <div class="min-w-0">
              <div class="mb-2 text-[11px] font-medium tracking-wide text-neutral-500 uppercase dark:text-fg-dim">
                {group}
              </div>
              <div class="divide-y divide-neutral-200 overflow-hidden rounded-xl border border-neutral-200 dark:divide-white/[0.06] dark:border-white/[0.08]">
                {#each events as event (event.key)}
                  <div class="grid gap-2 px-3.5 py-3 sm:grid-cols-[minmax(0,1fr)_minmax(10rem,14rem)] sm:items-center sm:gap-4">
                    <div class="min-w-0">
                      <div class="truncate text-[13px] font-medium text-black dark:text-fg">
                        {event.label}
                      </div>
                      <div class="text-[11px] text-neutral-500 dark:text-fg-dim">Topic ID</div>
                    </div>
                    <FormInput
                      id="{channel}_notifications_{event.key}_thread_id"
                      type="password"
                      placeholder="Optional"
                      aria-label="{event.label} topic ID"
                      {disabled}
                      bind:value={form[`${channel}_notifications_${event.key}_thread_id`]}
                    />
                  </div>
                {/each}
              </div>
            </div>
          {/each}
        </div>
      {/if}
    </SettingsSection>
  {/if}
</div>
