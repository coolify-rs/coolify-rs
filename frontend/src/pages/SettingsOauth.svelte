<script lang="ts">
  // Port of upstream livewire/settings-oauth.blade.php (App\Livewire\SettingsOauth) and
  // components/settings/layout.blade.php.
  import { onMount } from 'svelte';
  import Checkbox from '../components/Checkbox.svelte';
  import ErrorPage from '../components/ErrorPage.svelte';
  import FormInput from '../components/FormInput.svelte';
  import SettingsSection from '../components/SettingsSection.svelte';
  import UnsavedBar from '../components/UnsavedBar.svelte';
  import { ApiError } from '../lib/api';
  import { authUi, type OauthProvider } from '../lib/authApi';
  import { page } from '../lib/page.svelte';
  import { router } from '../lib/router.svelte';
  import { notify } from '../lib/toast.svelte';

  let { provider }: { provider?: string } = $props();

  let providers = $state<OauthProvider[]>([]);
  let saved = $state('');
  let disableRegistration = $state(false);
  let savedPolicy = $state(false);
  let errors = $state<Record<string, string[]>>({});
  let notFound = $state(false);
  let loaded = $state(false);
  let saving = $state(false);
  let active = $state(window.location.hash.slice(1).replace('-oauth-section', '') || provider || '');
  const highlighted = { ishighlighted: '' };

  const snapshot = () => JSON.stringify(providers);
  const dirty = $derived(loaded && (JSON.stringify(providers) !== saved || disableRegistration !== savedPolicy));

  async function load() {
    const d = await authUi.oauthSettings();
    if (d.redirect) {
      router.go(d.redirect, true);
      return;
    }
    if (provider && !d.providers.some((p) => p.provider === provider)) {
      notFound = true;
      return;
    }
    providers = d.providers;
    saved = snapshot();
    disableRegistration = d.disable_registration_when_oauth_enabled;
    savedPolicy = disableRegistration;
    active ||= d.providers[0]?.provider ?? '';
    loaded = true;
  }

  onMount(() => {
    load();
    page.navChildren = submenu;
    return () => (page.navChildren = null);
  });

  const error = (p: string, f: string) => errors[`oauth_settings_map.${p}.${f}`]?.[0];

  function select(e: MouseEvent, p: string) {
    e.preventDefault();
    active = p;
    history.replaceState(null, '', `#${p}-oauth-section`);
    document.getElementById(`${p}-oauth-section`)?.scrollIntoView({ behavior: 'smooth', block: 'start' });
  }

  function failed(err: unknown, toast: boolean) {
    // `handleError` shows a toast; only `submit` lets a validation error reach the fields.
    if (!toast && err instanceof ApiError && Object.keys(err.errors).length) errors = err.errors;
    else notify.failure(err instanceof Error ? err.message : String(err));
  }

  /** `submit`. */
  async function submit() {
    if (saving) return;
    saving = true;
    errors = {};
    try {
      if (provider) {
        const p = providers.find((x) => x.provider === provider);
        if (p) notify.success((await authUi.saveOauth(provider, p)).message);
      } else {
        const res = await authUi.saveOauthAll({ providers, disable_registration_when_oauth_enabled: disableRegistration });
        if (res.error) notify.failure(res.error);
        notify.success(res.message);
      }
      await load();
    } catch (err) {
      failed(err, false);
      if (!(err instanceof ApiError && Object.keys(err.errors).length)) await load();
    } finally {
      saving = false;
    }
  }

  /** `toggleProvider`, after the browser's check of the section's required fields. */
  async function toggle(e: MouseEvent, p: OauthProvider) {
    if (!p.enabled) {
      const section = (e.currentTarget as HTMLElement).closest('section');
      const invalid = [...(section?.querySelectorAll<HTMLInputElement>('[required]') ?? [])].find((f) => !f.checkValidity());
      if (invalid) {
        invalid.reportValidity();
        return;
      }
    }
    errors = {};
    try {
      const res = await authUi.toggleOauth(p.provider, p);
      notify.success(res.message);
      await load();
    } catch (err) {
      failed(err, true);
      await load();
    }
  }

  /** `saveRegistrationPolicy`. */
  async function savePolicy(value: boolean) {
    try {
      notify.success((await authUi.saveRegistrationPolicy(value)).message);
      savedPolicy = value;
    } catch (err) {
      failed(err, true);
    }
  }

  const text = (v: string | number | null) => (v === null ? '' : String(v));
  /** A field that was null stays null while it is empty, as Livewire's model does. */
  function bindText(p: OauthProvider, key: 'client_id' | 'client_secret' | 'redirect_uri' | 'tenant' | 'base_url' | 'custom_label') {
    return {
      get value() {
        return text(p[key]);
      },
      set value(v: string) {
        p[key] = v === '' && p[key] === null ? null : v;
      },
    };
  }
</script>

{#snippet submenu()}
  <div>
    <nav aria-label="OAuth providers" class="grid gap-0.5 py-1">
      {#each providers as p (p.provider)}
        <a href="#{p.provider}-oauth-section" class="menu-item min-h-8! py-1! text-[12px]! {active === p.provider ? 'menu-item-active' : ''}" onclick={(e) => select(e, p.provider)}>
          <span class="menu-item-icon shrink-0">
            <span class="block size-4 bg-current" style="mask: url('/svgs/{p.provider}.svg') center / contain no-repeat; -webkit-mask: url('/svgs/{p.provider}.svg') center / contain no-repeat;"></span>
          </span>
          <span class="menu-item-label">{p.label}</span>
        </a>
      {/each}
    </nav>
  </div>
{/snippet}

{#snippet bound(p: OauthProvider, key: 'client_id' | 'client_secret' | 'redirect_uri' | 'tenant' | 'base_url' | 'custom_label', props: Record<string, unknown>)}
  {@const b = bindText(p, key)}
  <FormInput id="oauth_settings_map.{p.provider}.{key}" error={error(p.provider, key)} {...props} bind:value={b.value} />
{/snippet}

{#if notFound}
  <ErrorPage code={404} />
{:else if loaded}
  <section class="application-settings-workspace w-full max-w-none">
    <header class="settings-mobile-header xl:hidden">
      <h1 class="settings-mobile-title">Instance Settings</h1>
      <p class="settings-mobile-description">Configure global settings for this coolify-rs instance.</p>
    </header>
    <div class="min-w-0">
      <form
        class="application-settings-form flex w-full min-w-0 flex-col gap-6"
        onsubmit={(e) => {
          e.preventDefault();
          submit();
        }}
      >
        <UnsavedBar {dirty} {saving} onsave={submit} onreset={load} />

        <SettingsSection title="Registration" description="Control password registration when an OAuth provider is available.">
          <Checkbox
            id="disable_registration_when_oauth_enabled"
            label="Disable password registration when OAuth is enabled"
            helper="OAuth providers can still create users when registration is enabled for that provider."
            bind:checked={disableRegistration}
            onchange={savePolicy}
          />
        </SettingsSection>

        {#each providers as p (p.provider)}
          <div class="contents" data-provider={p.provider}>
            <SettingsSection id="{p.provider}-oauth-section" title={p.label}>
              {#snippet actions()}
                <div>
                  {#if p.enabled}
                    <button class="button" type="button" onclick={(e) => toggle(e, p)}>Disable</button>
                  {:else}
                    <button class="button" type="button" onclick={(e) => toggle(e, p)} {...highlighted}>Enable</button>
                  {/if}
                </div>
              {/snippet}
              <div class="grid gap-4 lg:grid-cols-2">
                {@render bound(p, 'redirect_uri', { placeholder: p.default_redirect_uri, label: 'Redirect URI' })}
                {#if p.provider === 'oidc'}
                  {@render bound(p, 'base_url', {
                    label: 'Issuer URL',
                    required: true,
                    helper: 'OpenID Provider issuer URL, for example https://example.okta.com. coolify-rs uses it to discover the authorization, token, userinfo, and JWKS endpoints. For Microsoft Entra ID, use https://login.microsoftonline.com/&lt;tenant ID&gt;/v2.0 and add the optional ID token claims email and xms_edov to the app registration.',
                  })}
                {/if}
                {@render bound(p, 'client_id', { label: 'Client ID', required: true })}
                {@render bound(p, 'client_secret', { type: 'password', label: 'Client secret', autocomplete: 'new-password', required: true })}
                {#if p.provider === 'oidc'}
                  <FormInput id="oauth_settings_map.oidc.scopes" label="Scopes" helper="Must include openid. Common scopes are openid email profile groups." error={error('oidc', 'scopes')} bind:value={p.scopes} />
                  <FormInput id="oauth_settings_map.oidc.clock_skew_seconds" type="number" label="Clock skew (seconds)" error={error('oidc', 'clock_skew_seconds')} bind:value={p.clock_skew_seconds as string} />
                  <div class="lg:col-span-2">
                    {@render bound(p, 'custom_label', { label: 'Login button label', placeholder: 'Login with SSO' })}
                  </div>
                {/if}
                {#if p.provider === 'azure'}
                  {@render bound(p, 'tenant', { label: 'Tenant', required: true })}
                {/if}
                {#if p.provider === 'google'}
                  {@render bound(p, 'tenant', {
                    label: 'Hosted domain',
                    helper: 'Optional Google Workspace domain, for example example.com. Only accounts from this domain can sign in. Use * to allow any Workspace account but no personal Google accounts.',
                  })}
                {/if}
                {#if ['authentik', 'clerk', 'zitadel', 'gitlab'].includes(p.provider)}
                  {@render bound(p, 'base_url', { label: 'Base URL', required: ['authentik', 'clerk'].includes(p.provider) })}
                {/if}
              </div>

              <div class="mt-4 grid gap-3 lg:grid-cols-2">
                {#if p.provider === 'oidc'}
                  <Checkbox id="oauth_settings_map.oidc.allow_registration" label="Allow OIDC user creation" helper="Allow a successful OIDC login to create a user when password registration is disabled." bind:checked={p.allow_registration} />
                  <Checkbox id="oauth_settings_map.oidc.require_email_verified" label="Require verified email" bind:checked={p.require_email_verified} />
                  <Checkbox id="oauth_settings_map.oidc.use_pkce" label="Use PKCE" bind:checked={p.use_pkce} />
                {/if}
                <Checkbox
                  id="oauth_settings_map.{p.provider}.auto_join_root_team"
                  label="Auto-join new users to Root team"
                  helper="Add newly-created OAuth users to the Root team as members without creating a personal team."
                  bind:checked={p.auto_join_root_team}
                />
              </div>
            </SettingsSection>
          </div>
        {/each}
      </form>
    </div>
  </section>
{/if}
