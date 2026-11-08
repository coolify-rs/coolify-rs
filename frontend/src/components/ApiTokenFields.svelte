<script lang="ts">
  // Port of upstream livewire/security/partials/api-token-fields.blade.php: the fields of an
  // ApiTokenForm (the new token form and the edit modal).
  import Reicon from './Reicon.svelte';
  import Helper from './Helper.svelte';
  import FormInput from './FormInput.svelte';
  import Listbox from './Listbox.svelte';
  import Checkbox from './Checkbox.svelte';
  import { notify } from '../lib/toast.svelte';
  import type { ScopeOption, TokenForm, TokenPermissionRights } from '../lib/securityApi';

  let {
    id,
    form = $bindable(),
    expiryOptions,
    scopeOptions,
    scopeLabels,
    canUse,
  }: {
    id: string;
    form: TokenForm;
    expiryOptions: { value: string; label: string }[];
    scopeOptions: ScopeOption[];
    scopeLabels: Record<string, string>;
    canUse: TokenPermissionRights;
  } = $props();

  let permissionsOpen = $state(false);
  let permissionRoot: HTMLDivElement | undefined = $state();
  let panelStyle = $state('');
  let trigger: HTMLButtonElement | undefined = $state();
  let panel: HTMLDivElement | undefined = $state();

  /** A fixed panel escapes the scrolling modal body, so it is not cut off. */
  function positionPanel() {
    if (!trigger || !panel) return;
    const gap = 4;
    const edge = 12;
    const t = trigger.getBoundingClientRect();
    const height = Math.min(panel.scrollHeight, 256);
    const fitsBelow = window.innerHeight - t.bottom - gap >= height;
    const top = fitsBelow ? t.bottom + gap : Math.max(edge, t.top - gap - height);
    panelStyle = `top: ${top}px; left: ${t.left}px; width: ${t.width}px; min-width: ${t.width}px; max-width: ${t.width}px;`;
  }

  function togglePanel() {
    permissionsOpen = !permissionsOpen;
    if (permissionsOpen) requestAnimationFrame(positionPanel);
  }

  const isRoot = $derived(form.permissions.includes('root'));
  const summary = $derived(
    form.permissions.map((p) => p.split(':').map((w) => w.charAt(0).toUpperCase() + w.slice(1)).join(' ')).join(', '),
  );

  const denied: Record<string, [keyof TokenPermissionRights, string, string[]]> = {
    root: ['root', 'You do not have permission to use root permissions.', ['root']],
    write: ['write', 'You do not have permission to use write permissions.', ['write', 'write:sensitive']],
    'write:sensitive': ['write', 'You do not have permission to use write permissions.', ['write', 'write:sensitive']],
    deploy: ['deploy', 'You do not have permission to use deploy permissions.', ['deploy']],
    'read:sensitive': ['sensitive', 'You do not have permission to use read:sensitive permissions.', ['read:sensitive']],
    delete: ['delete', 'You do not have permission to use delete permissions.', ['delete']],
  };

  /** `ApiTokenForm::updatedPermissions`. */
  function toggle(permission: string, checked: boolean) {
    const old = form.permissions;
    let next = checked ? [...old, permission] : old.filter((p) => p !== permission);
    const rule = denied[permission];
    if (checked && rule && !canUse[rule[0]]) {
      notify.failure(rule[1]);
      form.permissions = next.filter((p) => !rule[2].includes(p));
      return;
    }
    if (checked && permission === 'root') next = ['root'];
    else if (checked && permission === 'read:sensitive' && !next.includes('read')) next.push('read');
    else if (next.length === 0) next = ['read'];
    form.permissions = next.sort();
  }

  const types = ['project', 'environment', 'resource'];
  const isSelectedMode = $derived(form.scopeMode === 'selected');
  const sorted = $derived(
    [...form.selectedScopes].sort(
      (a, b) =>
        types.indexOf(a.split(':')[0]) - types.indexOf(b.split(':')[0]) || (scopeLabels[a] ?? a).localeCompare(scopeLabels[b] ?? b),
    ),
  );

  // A selected project already covers its environments and resources, and a selected environment its resources.
  const parentsOf = $derived(Object.fromEntries(scopeOptions.map((o) => [o.value, o.parents ?? []])) as Record<string, string[]>);
  const coveredBy = (value: string, selected: string[]) => (parentsOf[value] ?? []).find((p) => selected.includes(p));
  const listOptions = $derived(
    scopeOptions.map((o) => {
      const parent = o.header ? undefined : coveredBy(o.value, form.selectedScopes);
      return { value: o.value, label: o.label, header: o.header, disabled: Boolean(parent), description: parent ? `Included in ${scopeLabels[parent]}` : undefined };
    }),
  );
  $effect(() => {
    const selected = form.selectedScopes;
    if (selected.some((v) => coveredBy(v, selected))) form.selectedScopes = selected.filter((v) => !coveredBy(v, selected));
  });

  const scopeModes = $derived([
    { value: 'team' as const, label: 'All resources in the team', disabled: !canUse.team_scope },
    { value: 'selected' as const, label: 'Selected projects, environments, or resources' },
  ]);
</script>

<svelte:window
  onclick={(e) =>
    permissionsOpen && permissionRoot && !permissionRoot.contains(e.target as Node) && !panel?.contains(e.target as Node) && (permissionsOpen = false)}
  onkeydown={(e) => e.key === 'Escape' && (permissionsOpen = false)}
  onresize={() => permissionsOpen && positionPanel()}
/>

<div class="grid gap-4 lg:grid-cols-2">
  <FormInput required id="{id}.description" label="Description" placeholder="CI deployment token" bind:value={form.description} />
  <Listbox id="{id}.expiresInDays" label="Expires in" options={expiryOptions} bind:value={form.expiresInDays} />
</div>

<div class="mt-4 border-t border-neutral-200 pt-3 dark:border-white/[0.08]">
  <div class="mb-2 flex items-center gap-2">
    <h4 class="text-[12px] font-semibold text-black dark:text-fg">Permissions</h4>
    <Helper helper="Only grant the abilities this token needs." />
  </div>
  <div class="relative" bind:this={permissionRoot}>
    <button type="button" bind:this={trigger} class="listbox-trigger" onclick={togglePanel} aria-haspopup="listbox" aria-expanded={permissionsOpen}>
      <span class="truncate">{'Selected permissions: ' + summary}</span>
      <Reicon name="chevron-down" class="size-3.5 shrink-0 opacity-60" />
    </button>

    {#if permissionsOpen}
      <div bind:this={panel} class="listbox-panel" style="position: fixed; z-index: 9999; {panelStyle}" role="listbox">
        <div class="listbox-option p-0!">
          <Checkbox id="permission-root" label="Root" fullWidth helper="Full access to every API operation." checked={form.permissions.includes('root')} disabled={!canUse.root} onchange={(c) => toggle('root', c)} />
        </div>
        <div class="listbox-option p-0!">
          <Checkbox id="permission-write" label="Write" fullWidth helper="Create and update resources." checked={form.permissions.includes('write')} disabled={isRoot || !canUse.write} onchange={(c) => toggle('write', c)} />
        </div>
        <div class="listbox-option p-0!">
          <Checkbox
            id="permission-delete"
            label="Delete"
            fullWidth
            helper="Delete resources and their settings (every DELETE request)."
            checked={form.permissions.includes('delete')}
            disabled={isRoot || !canUse.delete}
            onchange={(c) => toggle('delete', c)}
          />
        </div>
        <div class="listbox-option p-0!">
          <Checkbox
            id="permission-deploy"
            label="Deploy"
            fullWidth
            helper="Deploy, start, stop, and restart resources. With Write, also apply changes that run on servers immediately, such as scheduled tasks, proxy configuration, and imports."
            checked={form.permissions.includes('deploy')}
            disabled={isRoot || !canUse.deploy}
            onchange={(c) => toggle('deploy', c)}
          />
        </div>
        <div class="listbox-option p-0!">
          <Checkbox id="permission-read" label="Read" fullWidth helper="Read non-sensitive resource data." checked={form.permissions.includes('read')} disabled={isRoot} onchange={(c) => toggle('read', c)} />
        </div>
        <div class="listbox-option p-0!">
          <Checkbox
            id="permission-read-sensitive"
            label="Read sensitive data"
            fullWidth
            helper="Include secrets, logs, passwords, and Compose content."
            checked={form.permissions.includes('read:sensitive')}
            disabled={isRoot || !canUse.sensitive}
            onchange={(c) => toggle('read:sensitive', c)}
          />
        </div>
      </div>
    {/if}
  </div>
</div>

<div class="mt-4 border-t border-neutral-200 pt-3 dark:border-white/[0.08]">
  <div class="mb-2 flex items-center gap-2">
    <h4 class="text-[12px] font-semibold text-black dark:text-fg">Access</h4>
    <Helper
      helper="A limited token sees only the selected projects, environments, and resources, and cannot use team-level endpoints such as servers, keys, or notifications."
    />
  </div>
  <div class="grid gap-4 lg:grid-cols-2">
    <Listbox id="{id}.scopeMode" label="Resources" options={scopeModes} bind:value={form.scopeMode} />
    <div class="min-w-0">
      <Listbox
        id="{id}.selectedScopes"
        label="Selected"
        multiple
        searchable
        countLabel="resource"
        disabled={!isSelectedMode}
        placeholder="Choose projects, environments, or resources"
        emptyText="No projects or resources available."
        options={listOptions}
        bind:values={form.selectedScopes}
      />
    </div>
  </div>
  {#if isSelectedMode && sorted.length > 0}
    <div class="mt-3 divide-y divide-neutral-200 rounded-md border border-neutral-200 dark:divide-white/[0.06] dark:border-white/[0.08]">
      {#each sorted as scope (scope)}
        <div class="flex min-h-8 items-center gap-3 py-1 pl-3 pr-1.5 text-[12px]">
          <span class="w-20 shrink-0 text-[11px] capitalize text-neutral-500 dark:text-fg-dim">{scope.split(':')[0]}</span>
          <span class="min-w-0 flex-1 truncate text-black dark:text-fg">{scopeLabels[scope] ?? scope}</span>
          <button
            type="button"
            class="icon-button shrink-0"
            title="Remove"
            aria-label={'Remove ' + (scopeLabels[scope] ?? scope)}
            onclick={() => (form.selectedScopes = form.selectedScopes.filter((v) => v !== scope))}
          >
            <Reicon name="x" class="size-3.5" />
          </button>
        </div>
      {/each}
    </div>
  {/if}
</div>
