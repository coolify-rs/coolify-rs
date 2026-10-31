<script lang="ts">
  // Port of upstream components/forms/env-var-input.blade.php: the password input with the
  // `{{ scope.KEY }}` autocomplete. The vault scope appears when the resource has a secret manager
  // source (`hasVaultSource`); its keys are fetched when a vault reference is typed.
  import { tick } from 'svelte';
  import Reicon from '../Reicon.svelte';
  import { request } from '../../lib/api';
  import type { AvailableVars } from '../../lib/sharedApi';

  let {
    id,
    value = $bindable(''),
    availableVars,
    disabled = false,
    required = false,
    placeholder = '',
    password = true,
    scopeUrls = {},
  }: {
    id: string;
    value?: string;
    availableVars: AvailableVars;
    disabled?: boolean;
    required?: boolean;
    placeholder?: string;
    password?: boolean;
    scopeUrls?: Record<string, string>;
  } = $props();

  interface Suggestion {
    type: 'scope' | 'variable';
    value: string;
    display: string;
  }

  let input: HTMLInputElement | undefined = $state();
  let shown = $state(false);
  let showDropdown = $state(false);
  let suggestions = $state<Suggestion[]>([]);
  let selectedIndex = $state(0);
  let cursor = 0;
  let currentScope = $state<string | null>(null);

  const availableScopes = $derived([
    ...(['team', 'project', 'environment', 'server'] as const).filter((s) => availableVars[s]?.length > 0),
    ...(availableVars.vault_keys_url ? (['vault'] as const) : []),
  ] as string[]);
  let vaultKeys = $state<string[] | undefined>(undefined);
  let vaultKeysLoading = $state(false);

  async function loadVaultKeys() {
    if (vaultKeysLoading || !availableVars.vault_keys_url) return;
    vaultKeysLoading = true;
    try {
      vaultKeys = (await request<{ keys: string[] }>('GET', availableVars.vault_keys_url)).keys;
    } catch {
      vaultKeys = [];
    } finally {
      vaultKeysLoading = false;
      handleInput();
    }
  }

  function handleInput() {
    if (!input) return;
    const text = input.value || '';
    cursor = input.selectionStart || 0;
    const before = text.substring(0, cursor);
    const last = before.lastIndexOf('{{');
    if (last === -1 || (last > 0 && before[last - 1] === '{')) {
      showDropdown = false;
      return;
    }
    const after = before.substring(last);
    if (after.includes('}}')) {
      showDropdown = false;
      return;
    }
    const content = after.substring(2).trim();
    if (content === '') {
      currentScope = null;
      if (availableScopes.length === 0) {
        showDropdown = false;
        return;
      }
      suggestions = availableScopes.map((s) => ({ type: 'scope', value: s, display: s }));
      selectedIndex = 0;
      showDropdown = true;
    } else if (content.includes('.')) {
      const [scope, partial] = content.split('.');
      if (!availableScopes.includes(scope)) {
        showDropdown = false;
        return;
      }
      currentScope = scope;
      if (scope === 'vault' && vaultKeys === undefined) loadVaultKeys();
      const vars = scope === 'vault' ? (vaultKeys ?? []) : ((availableVars[scope as 'team'] as string[]) ?? []);
      const filtered = vars.filter((v) => v.toLowerCase().includes((partial || '').toLowerCase()));
      suggestions = filtered.map((v) => ({ type: 'variable', value: v, display: `${scope}.${v}` }));
      selectedIndex = 0;
      showDropdown = filtered.length > 0;
    } else {
      currentScope = null;
      const filtered = availableScopes.filter((s) => s.toLowerCase().includes(content.toLowerCase()));
      suggestions = filtered.map((s) => ({ type: 'scope', value: s, display: s }));
      selectedIndex = 0;
      showDropdown = filtered.length > 0;
    }
  }

  async function select(s: Suggestion) {
    if (!input) return;
    const text = input.value || '';
    const before = text.substring(0, cursor);
    const rest = text.substring(cursor);
    const last = before.lastIndexOf('{{');
    if (s.type === 'scope') {
      value = `${text.substring(0, last)}{{ ${s.value}.${rest}`;
      cursor = last + 3 + s.value.length + 1;
      await tick();
      input.setSelectionRange(cursor, cursor);
      input.focus();
      handleInput();
    } else {
      value = `${text.substring(0, last)}{{ ${s.display} }}${rest}`;
      cursor = last + 3 + s.display.length + 3;
      showDropdown = false;
      selectedIndex = 0;
      await tick();
      input.setSelectionRange(cursor, cursor);
      input.focus();
    }
  }

  function onkeydown(e: KeyboardEvent) {
    if (!showDropdown || suggestions.length === 0) return;
    if (e.key === 'ArrowDown') {
      e.preventDefault();
      selectedIndex = (selectedIndex + 1) % suggestions.length;
    } else if (e.key === 'ArrowUp') {
      e.preventDefault();
      selectedIndex = selectedIndex <= 0 ? suggestions.length - 1 : selectedIndex - 1;
    } else if (e.key === 'Enter') {
      e.preventDefault();
      if (suggestions[selectedIndex]) select(suggestions[selectedIndex]);
    } else if (e.key === 'Escape') {
      showDropdown = false;
    }
  }

  function outside(node: HTMLElement) {
    const handle = (e: MouseEvent) => {
      if (!node.contains(e.target as Node)) showDropdown = false;
    };
    document.addEventListener('click', handle);
    return { destroy: () => document.removeEventListener('click', handle) };
  }
</script>

<div class="relative" use:outside>
  <input
    bind:this={input}
    bind:value
    oninput={handleInput}
    {onkeydown}
    onclick={handleInput}
    autocomplete="new-password"
    type={password && !shown ? 'password' : 'text'}
    class="input {password ? 'input-with-password-toggle' : ''} {!password || shown ? (disabled ? '' : 'truncate') : ''}"
    {id}
    name={id}
    {required}
    {disabled}
    {placeholder}
  />
  {#if password}
    <button
      type="button"
      onclick={() => (shown = !shown)}
      class="password-toggle flex absolute inset-y-0 right-0 z-10 items-center pr-2 cursor-pointer text-neutral-500 hover:text-black dark:text-neutral-400 dark:hover:text-white"
      aria-label="Toggle password visibility"
    >
      {#if shown}<Reicon name="eye-off2" class="size-[18px]" />{:else}<Reicon name="eye" class="size-[18px]" />{/if}
    </button>
  {/if}
  {#if showDropdown}
    <div class="listbox-panel top-full! z-[60]! mt-1! w-full! min-w-0! max-w-full!" role="listbox">
      {#if suggestions.length === 0 && currentScope}
        <div class="px-2 py-2 text-sm text-neutral-500 dark:text-fg-dim">
          <div>No shared variables found in <span class="font-semibold">{currentScope}</span> scope.</div>
          <a href={scopeUrls[currentScope] ?? scopeUrls['default']} class="mt-1 inline-block text-xs text-coollabs hover:underline dark:text-brand-light" target="_blank">
            Add <span>{currentScope}</span> variables →
          </a>
        </div>
      {/if}
      {#if suggestions.length > 0}
        <div class="max-h-48 overflow-y-auto" style="scrollbar-width: thin;">
          {#each suggestions as s, i (s.display)}
            <!-- svelte-ignore a11y_click_events_have_key_events -->
            <div
              id="suggestion-{i}"
              onclick={() => select(s)}
              class="listbox-option justify-start! gap-2.5! {i === selectedIndex ? 'bg-neutral-100 dark:bg-white/[0.08]' : ''}"
              role="option"
              tabindex="-1"
              aria-selected={i === selectedIndex}
            >
              {#if s.type === 'scope'}
                <span class="rounded-md border border-warning/25 bg-warning/10 px-1.5 py-0.5 text-[10px] font-semibold tracking-wide text-warning">SCOPE</span>
              {:else}
                <span class="rounded-md border border-emerald-500/25 bg-emerald-500/10 px-1.5 py-0.5 text-[10px] font-semibold tracking-wide text-emerald-600 dark:text-emerald-400">VAR</span>
              {/if}
              <span class="min-w-0 truncate font-mono text-sm">{s.display}</span>
            </div>
          {/each}
        </div>
      {/if}
    </div>
  {/if}
</div>
