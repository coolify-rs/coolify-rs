<script lang="ts">
  // Port of upstream components/resource-logo.blade.php with resource_logo()
  // (bootstrap/helpers/services.php): the database engine logo, or how an
  // application is built (with the favicon of its first domain, shown once it
  // loads), or the resource type icon when there is no logo or it fails to
  // load (resourceLogoLoaded / resourceLogoError in resources/js/app.js).
  import Reicon from './Reicon.svelte';

  let {
    databaseType = '',
    application = null,
    iconClass = 'size-4',
  }: {
    databaseType?: string;
    /** For an application: its build pack, source type, domains and basic auth flag. */
    application?: { buildPack: string | null; sourceType: string | null; fqdn: string | null; basicAuth: boolean } | null;
    iconClass?: string;
  } = $props();

  // Engine => [logo name, has a dark variant].
  const logos: Record<string, [string, boolean]> = {
    postgresql: ['postgres', false],
    mysql: ['mysql', false],
    mariadb: ['mariadb', false],
    mongodb: ['mongodb', false],
    redis: ['redis', false],
    keydb: ['keydb', true],
    dragonfly: ['dragonfly', true],
    clickhouse: ['clickhouse', false],
    sqlite: ['sqlite', false],
  };

  function applicationLogo(a: NonNullable<typeof application>): [string, boolean] {
    if (a.buildPack === 'dockerimage') return ['docker-image', false];
    if (a.buildPack === 'dockerfile') return ['dockerfile', false];
    if (a.buildPack === 'dockercompose') return ['docker-compose', false];
    if (a.sourceType === 'App\\Models\\GithubApp') return ['github-app', true];
    if (a.sourceType === 'App\\Models\\GitlabApp') return ['gitlab-app', false];
    return ['public-repo', false];
  }

  /** resource_favicon_url(): `{scheme}://{host}[:port]/favicon.ico` of the first domain. */
  function faviconUrl(fqdn: string | null): string | null {
    const first = (fqdn ?? '')
      .split(',')
      .map((u) => u.trim())
      .find((u) => u !== '');
    if (!first) return null;
    try {
      const u = new URL(first);
      const scheme = u.protocol.replace(':', '').toLowerCase();
      if (!['http', 'https'].includes(scheme) || u.hostname === '' || u.hostname.includes('*')) return null;
      return `${scheme}://${u.hostname}${u.port ? `:${u.port}` : ''}/favicon.ico`;
    } catch {
      return null;
    }
  }

  const logo = $derived(application ? applicationLogo(application) : (logos[databaseType.replace(/^standalone-/, '')] ?? null));
  const favicon = $derived(application && !application.basicAuth ? faviconUrl(application.fqdn) : null);
  const icon = $derived(application ? 'browser-code' : 'database');
  const url = (name: string) => `/svgs/resources/${name}.svg`;
  let failed = $state(false);
  let faviconLoaded = $state(false);
  let faviconFailed = $state(false);
</script>

<span class="contents" data-resource-logo>
  {#if logo && !failed}
    <span class="flex size-full items-center justify-center" data-resource-logo-image hidden={faviconLoaded}>
      <img src={url(logo[0])} alt="" onerror={() => (failed = true)} class={['size-full object-contain', logo[1] && 'dark:hidden']} />
      {#if logo[1]}
        <img src={url(`${logo[0]}-dark`)} alt="" onerror={() => (failed = true)} class="hidden size-full object-contain dark:block" />
      {/if}
    </span>
  {/if}
  {#if favicon && !faviconFailed}
    <img
      src={favicon}
      alt=""
      hidden={!faviconLoaded}
      data-resource-logo-favicon
      onload={() => (faviconLoaded = true)}
      onerror={() => (faviconFailed = true)}
      class="size-full object-contain p-1"
    />
  {/if}
  {#if (!logo || failed) && !faviconLoaded}
    <Reicon name={icon} class={iconClass} />
  {/if}
</span>
