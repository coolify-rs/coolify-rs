<script lang="ts">
  // Port of upstream livewire/team/member/access.blade.php: the access grants of a member with
  // the member role, or of an invited member (then saving hands the grants back to the invite form).
  import Modal from './Modal.svelte';
  import Callout from './Callout.svelte';
  import Listbox from './Listbox.svelte';
  import Reicon from './Reicon.svelte';
  import { ApiError } from '../lib/api';
  import { notify } from '../lib/toast.svelte';
  import { teamsUi, type AccessOptions, type AccessRow, type StoredGrant } from '../lib/teamsApi';

  let {
    title,
    summary = null,
    forInvitation = false,
    memberId = null,
    rows: initialRows,
    onclose,
    onsaved,
    onchosen,
  }: {
    title: string;
    summary?: string | null;
    forInvitation?: boolean;
    memberId?: number | null;
    rows: AccessRow[];
    onclose: () => void;
    /** A member's grants were stored. */
    onsaved?: () => void;
    /** `invitation-access-chosen`. */
    onchosen?: (access: StoredGrant[], summary: string) => void;
  } = $props();

  let options = $state<AccessOptions | null>(null);
  // svelte-ignore state_referenced_locally
  let rows = $state<AccessRow[]>(initialRows.map((r) => ({ ...r, targets: [...r.targets], levels: { ...r.levels } })));
  let errors = $state<Record<string, string[]>>({});
  let saving = $state(false);
  let developerOpen = $state(false);

  teamsUi.accessOptions().then((o) => (options = o));

  const presetInfo: [string, string, string][] = [
    ['viewer', 'Viewer', 'Read the whole team.'],
    ['developer', 'Developer', 'Write and deploy in one project.'],
    ['operator', 'Operator', 'Write, deploy, and delete in the whole team.'],
    ['full', 'Full access', 'Every level in the whole team.'],
  ];
  const presetCard = 'flex w-full flex-col items-start gap-0.5 rounded-lg border px-3 py-2 text-left transition-colors';
  const presetIdle = 'border-neutral-200 hover:bg-neutral-100/80 dark:border-white/[0.08] dark:hover:bg-white/[0.035]';
  const presetActive = 'border-coollabs bg-coollabs/5 dark:border-brand-light/60 dark:bg-brand/10';

  const key = () => Math.random().toString(36).slice(2, 12);
  function row(targets: string[], levels: string[]): AccessRow {
    return { key: key(), targets, levels: Object.fromEntries((options?.levels ?? []).map((l) => [l.name, levels.includes(l.name)])) };
  }
  const checked = (r: AccessRow) =>
    (options?.levels ?? [])
      .filter((l) => r.levels[l.name])
      .map((l) => l.name)
      .sort();
  function applyPreset(name: string, projectUuid: string | null = null) {
    const preset = options?.presets[name];
    if (!preset) return;
    rows = [row(preset.targets.map((t) => t.replace('__project__', projectUuid ?? '')), checked(preset))];
  }
  function includedBy(r: AccessRow, name: string): string | null {
    const parent = (options?.levels ?? [])
      .filter((l) => r.levels[l.name])
      .sort((a, b) => b.includes.length - a.includes.length)
      .find((l) => l.name !== name && l.includes.includes(name));
    return parent?.label ?? null;
  }
  const activePreset = $derived.by(() => {
    if (!options || rows.length !== 1 || rows[0].targets.length !== 1) return null;
    const target = String(rows[0].targets[0]);
    const levels = checked(rows[0]).join(',');
    return (
      Object.keys(options.presets).find((name) => {
        const preset = options!.presets[name];
        const presetTarget = preset.targets[0];
        const sameTarget = presetTarget.startsWith('project:') ? target.startsWith('project:') : target === presetTarget;
        return sameTarget && checked(preset).join(',') === levels;
      }) ?? null
    );
  });

  /** `save()`. */
  async function save() {
    if (saving) return;
    saving = true;
    errors = {};
    try {
      if (forInvitation) {
        const r = await teamsUi.invitationAccess(rows);
        onchosen?.(r.access, r.summary);
      } else if (memberId !== null) {
        const r = await teamsUi.saveMemberAccess(memberId, rows);
        notify.success(r.message);
        onsaved?.();
      }
      onclose();
    } catch (e) {
      if (e instanceof ApiError && e.status === 422 && Object.keys(e.errors).length) errors = e.errors;
      else notify.failure(e instanceof ApiError ? e.message : String(e));
    } finally {
      saving = false;
    }
  }

  const scopeList = $derived((options?.scope_options ?? []).map((o) => ({ value: o.value, label: o.label, header: o.header })));
</script>

<Modal {title} subtitle={summary ? `Current access: ${summary}` : 'The member gets these scopes when they accept the invitation.'} {onclose} focusFirstField={false}>
  <div class="flex flex-col gap-5 lg:w-3xl">
    <section class="flex flex-col gap-2">
      <div>
        <h4 class="text-[13px] font-medium text-neutral-800 dark:text-fg">Start from a preset</h4>
        <p class="text-[12px] text-neutral-500 dark:text-fg-faint">A preset replaces the scopes below. You can change them after.</p>
      </div>
      <div class="grid grid-cols-1 gap-2 sm:grid-cols-2">
        {#each presetInfo as [preset, label, description] (preset)}
          {#if preset === 'developer'}
            {#if (options?.project_options.length ?? 0) > 0}
              <div class="relative">
                <button
                  type="button"
                  class="{presetCard} {activePreset === 'developer' ? presetActive : presetIdle}"
                  onclick={() => (developerOpen = !developerOpen)}
                  aria-haspopup="menu"
                  aria-expanded={developerOpen}
                >
                  <span class="flex w-full items-center justify-between gap-2 text-[13px] text-neutral-800 dark:text-fg">
                    {label}
                    <Reicon name="chevron-down" class="size-3 text-neutral-500 dark:text-fg-faint" />
                  </span>
                  <span class="text-[12px] text-neutral-500 dark:text-fg-faint">{description}</span>
                </button>
                {#if developerOpen}
                  <div role="menu" class="listbox-panel min-w-48! z-[100]! top-full! mt-1!">
                    {#each options?.project_options ?? [] as project (project.value)}
                      <button
                        type="button"
                        class="listbox-option justify-start!"
                        onclick={() => {
                          applyPreset('developer', project.value.replace(/^project:/, ''));
                          developerOpen = false;
                        }}
                      >
                        {project.label}
                      </button>
                    {/each}
                  </div>
                {/if}
              </div>
            {/if}
          {:else}
            <button type="button" class="{presetCard} {activePreset === preset ? presetActive : presetIdle}" onclick={() => applyPreset(preset)}>
              <span class="text-[13px] text-neutral-800 dark:text-fg">{label}</span>
              <span class="text-[12px] text-neutral-500 dark:text-fg-faint">{description}</span>
            </button>
          {/if}
        {/each}
      </div>
    </section>

    <section class="flex flex-col gap-2">
      <div>
        <h4 class="text-[13px] font-medium text-neutral-800 dark:text-fg">Scopes</h4>
        <p class="text-[12px] text-neutral-500 dark:text-fg-faint">Each scope gives levels on the whole team, projects, environments, or resources. Scopes add up.</p>
      </div>

      {#if rows.length === 0}
        <Callout type="warning" title="No access">
          {forInvitation ? 'The invited member' : 'This member'} will not see any project, environment, or resource in this team.
        </Callout>
      {/if}

      {#if options?.is_root_team && rows.some((row) => row.levels.Write || row.levels.Root)}
        <Callout type="warning" title="Write access can give root on the Coolify host">
          With write access on a resource that runs on the localhost server, a member can run a
          privileged container there. That gives root on the server that runs Coolify, and access to
          the data of all teams. Give write access in the root team only to people that you trust as
          instance admins.
        </Callout>
      {/if}

      {#if Object.keys(errors).length}
        <Callout type="danger" title="Check the scopes">
          {#each Object.entries(errors) as [k, messages] (k)}
            <p>Scope {Number(k.split('.')[1]) + 1}: {messages[0]}</p>
          {/each}
        </Callout>
      {/if}

      {#each rows as r (r.key)}
        <div class="flex flex-col gap-3 rounded-lg border border-neutral-200 p-3 dark:border-white/[0.08]">
          <div class="flex items-end gap-2">
            <div class="min-w-0 flex-1">
              <Listbox id="member-access-targets" label="Applies to" options={scopeList} placeholder="Choose scopes" multiple searchable countLabel="resource" bind:values={r.targets} />
            </div>
            <button type="button" class="icon-button mb-1 shrink-0" title="Remove scope" aria-label="Remove this scope" onclick={() => (rows = rows.filter((x) => x.key !== r.key))}>
              <Reicon name="trash" class="size-3.5" />
            </button>
          </div>

          <div class="grid grid-cols-1 gap-1.5 sm:grid-cols-2">
            {#each options?.levels ?? [] as level (level.name)}
              {@const parent = includedBy(r, level.name)}
              <label
                class="group flex items-start gap-2.5 rounded-md px-2.5 py-2 transition-colors {parent
                  ? 'cursor-not-allowed opacity-60'
                  : 'cursor-pointer hover:bg-neutral-100/80 dark:hover:bg-white/[0.035]'}"
              >
                <span class="relative mt-0.5 flex size-[18px] shrink-0">
                  <input
                    type="checkbox"
                    checked={parent !== null || r.levels[level.name]}
                    onchange={(e) => (r.levels[level.name] = (e.currentTarget as HTMLInputElement).checked)}
                    disabled={parent !== null}
                    class="peer absolute inset-0 z-10 m-0 h-full w-full cursor-pointer appearance-none opacity-0 disabled:cursor-not-allowed"
                  />
                  <span
                    class="pointer-events-none absolute inset-0 rounded-[5px] border border-neutral-300 bg-white transition-colors peer-checked:border-coollabs peer-checked:bg-coollabs peer-focus-visible:ring-2 peer-focus-visible:ring-coollabs/25 dark:border-white/[0.14] dark:bg-white/[0.045] dark:peer-checked:border-brand dark:peer-checked:bg-brand"
                  ></span>
                  <svg class="pointer-events-none absolute inset-0 m-auto size-3 text-white opacity-0 peer-checked:opacity-100" viewBox="0 0 12 12" fill="none" aria-hidden="true">
                    <path d="m2.25 6.15 2.35 2.3 5.15-5" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" />
                  </svg>
                </span>
                <span class="flex min-w-0 flex-col">
                  <span class="text-[13px] text-neutral-800 dark:text-fg">{level.label}</span>
                  <span class="text-[12px] text-neutral-500 dark:text-fg-faint">{parent ? `Included in ${parent}.` : level.description}</span>
                </span>
              </label>
            {/each}
          </div>
        </div>
      {/each}

      <div>
        <button type="button" class="button h-7! px-2.5! text-[11px]!" onclick={() => (rows = [...rows, row([], [])])}>
          <Reicon name="plus" class="size-3" />
          Add scope
        </button>
      </div>
    </section>
  </div>
  {#snippet footer()}
    <button type="button" class="button" onclick={onclose}>Cancel</button>
    <button type="button" class="button button-highlighted" disabled={saving} onclick={save}>Save</button>
  {/snippet}
</Modal>
