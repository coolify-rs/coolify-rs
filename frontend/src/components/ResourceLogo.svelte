<script lang="ts">
  // Port of upstream components/resource-logo.blade.php with resource_logo()
  // (bootstrap/helpers/services.php) for standalone databases: the engine
  // logo, or the database icon when there is none or it fails to load
  // (resourceLogoError in resources/js/app.js).
  import Reicon from './Reicon.svelte';

  let { databaseType, iconClass = 'size-4' }: { databaseType: string; iconClass?: string } = $props();

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
  const logo = $derived(logos[databaseType.replace(/^standalone-/, '')] ?? null);
  const url = (name: string) => `/svgs/resources/${name}.svg`;
  let failed = $state(false);
</script>

<span class="contents" data-resource-logo>
  {#if logo && !failed}
    <span class="flex size-full items-center justify-center" data-resource-logo-image>
      <img src={url(logo[0])} alt="" onerror={() => (failed = true)} class={['size-full object-contain', logo[1] && 'dark:hidden']} />
      {#if logo[1]}
        <img src={url(`${logo[0]}-dark`)} alt="" onerror={() => (failed = true)} class="hidden size-full object-contain dark:block" />
      {/if}
    </span>
  {:else}
    <Reicon name="database" class={iconClass} />
  {/if}
</span>
