<script lang="ts">
  // Port of upstream livewire/image-scan/packages.blade.php (ImageScan\Packages): the packages of
  // the images a resource runs, from the stored package lists, with the SBOM downloads. The list
  // is filtered and paged in the browser.
  import { untrack } from 'svelte';
  import Callout from '../Callout.svelte';
  import Dropdown from '../Dropdown.svelte';
  import Empty from '../Empty.svelte';
  import Pager from '../Pager.svelte';
  import Reicon from '../Reicon.svelte';
  import SettingsSection from '../SettingsSection.svelte';
  import { vulnerabilitiesApi as api, type PackagesData, type ResourceKind } from '../../lib/imageScanApi';
  import CheckMark from './CheckMark.svelte';
  import ImageSelect from './ImageSelect.svelte';
  import TableFilter from './TableFilter.svelte';
  import TableSearch from './TableSearch.svelte';

  let { kind, uuid, version = 0 }: { kind: ResourceKind; uuid: string; version?: number } = $props();

  const groupLabel = 'px-2 pb-1 pt-2 text-[10px] font-medium uppercase tracking-wider text-neutral-400 dark:text-fg-faint';

  let data = $state<PackagesData | null>(null);
  let search = $state('');
  let applied = $state('');
  let types = $state<string[]>([]);
  let images = $state<string[]>([]);
  let page = $state(1);
  let perPage = $state(10);

  $effect(() => {
    void kind;
    void uuid;
    void version;
    untrack(() => {
      api
        .packages(kind, uuid)
        .then((r) => (data = r))
        .catch(() => {});
    });
  });

  const typeOptions = $derived([...new Set((data?.packages ?? []).map((p) => p.type).filter((t) => t !== ''))].sort());
  const shownImages = $derived(images.filter((i) => data?.image_options.includes(i)));
  const filtered = $derived.by(() => {
    const q = applied.trim().toLowerCase();
    return (data?.packages ?? [])
      .filter((p) => types.length === 0 || types.includes(p.type))
      .filter((p) => shownImages.length === 0 || shownImages.includes(p.image))
      .filter((p) => q === '' || `${p.name} ${p.version} ${p.purl}`.toLowerCase().includes(q))
      .sort((a, b) => (a.name < b.name ? -1 : a.name > b.name ? 1 : a.version < b.version ? -1 : a.version > b.version ? 1 : 0));
  });
  const lastPage = $derived(Math.max(1, Math.ceil(filtered.length / perPage)));
  const shownPage = $derived(Math.min(Math.max(1, page), lastPage));
  const items = $derived(filtered.slice((shownPage - 1) * perPage, shownPage * perPage));

  function toggleType(type: string) {
    types = types.includes(type) ? types.filter((t) => t !== type) : [...types, type];
    page = 1;
  }
</script>

<div class="contents">
  {#if data?.has_scans}
    <SettingsSection
      id="packages-section"
      title="Packages and SBOM"
      collapsible
      flush
      description="The software packages that Coolify found in the image, with their versions and licenses. The SBOM (software bill of materials) is the full list of these packages in a standard format (CycloneDX or SPDX). Download it for audits or for other security tools."
    >
      {#snippet actions()}
        <Dropdown role="menu" panelClass="min-w-56! p-1!">
          {#snippet trigger({ open })}
            <button type="button" class="button normal-case!" aria-haspopup="menu" aria-expanded={open}>
              <Reicon name="download" class="size-3.5" />
              Download SBOM
              <Reicon name="chevron-down" class="size-3.5" />
            </button>
          {/snippet}
          {#snippet children({ close })}
            {#each data?.downloads ?? [] as download (download.image)}
              {#if (data?.downloads.length ?? 0) > 1}
                <span class="{groupLabel} block max-w-72 truncate normal-case tracking-normal" title={download.image}>{download.image}</span>
              {/if}
              {#if download.cyclonedx}
                <a class="listbox-option" role="menuitem" href={download.cyclonedx} download onclick={close}>CycloneDX 1.6 JSON</a>
              {/if}
              {#if download.spdx}
                <a class="listbox-option" role="menuitem" href={download.spdx} download onclick={close}>SPDX 2.3 JSON</a>
              {/if}
            {/each}
          {/snippet}
        </Dropdown>
      {/snippet}

      <div class="table-toolbar flex flex-col gap-2 sm:flex-row sm:flex-wrap sm:items-center border-b border-neutral-200 p-3 dark:border-white/[0.08]">
        <div class="min-w-0 w-full flex-1 sm:max-w-md">
          <TableSearch placeholder="Search packages" bind:value={search} onsearch={(v) => ((applied = v), (page = 1))} />
        </div>
        <div class="flex flex-wrap items-center gap-2 sm:ml-auto">
          <ImageSelect options={data.image_options} selected={shownImages} onselect={(image) => ((images = image === '' ? [] : [image]), (page = 1))} />
          {#if typeOptions.length > 0}
            <TableFilter activeCount={types.length} onreset={() => ((types = []), (page = 1))}>
              <span class={groupLabel}>Type</span>
              {#each typeOptions as option (option)}
                <button type="button" class="listbox-option" role="option" aria-selected={types.includes(option) ? 'true' : 'false'} onclick={() => toggleType(option)}>
                  <span class="truncate">{option}</span>
                  <CheckMark selected={types.includes(option)} />
                </button>
              {/each}
            </TableFilter>
          {/if}
        </div>
      </div>

      {#if data.truncated}
        <div class="border-b border-neutral-200 p-3 dark:border-white/[0.08]">
          <Callout type="info" title="Too many packages">Only the first 10,000 are shown.</Callout>
        </div>
      {/if}

      <div class="relative">
        {#if items.length === 0}
          {#if data.packages.length === 0}
            <Empty size="sm" title="No packages found." />
          {:else}
            <Empty size="sm" title="No matching packages" description="Try another search, or reset the filters." />
          {/if}
        {:else}
          <div class="data-table">
            <div class="data-table-header image-scan-sbom-grid {data.shows_image ? 'has-image' : ''}">
              <span>Package</span>
              <span>Version</span>
              <span class="image-scan-secondary">Type</span>
              {#if data.shows_image}
                <span class="image-scan-secondary">Image</span>
              {/if}
              <span class="image-scan-secondary">License</span>
            </div>
            {#each items as pkg (pkg.key)}
              <div class="data-table-row image-scan-sbom-grid border-b border-neutral-200 last:border-b-0 dark:border-white/[0.08] {data.shows_image ? 'has-image' : ''}">
                <span class="min-w-0 truncate text-[12px] font-medium text-neutral-950 dark:text-fg" title={pkg.purl || pkg.name}>{pkg.name}</span>
                <span class="min-w-0 truncate font-mono text-[11px] text-neutral-600 dark:text-fg-dim">{pkg.version}</span>
                <span class="image-scan-secondary min-w-0 truncate text-[11px] text-neutral-600 dark:text-fg-dim">{pkg.type}</span>
                {#if data.shows_image}
                  <span class="image-scan-secondary min-w-0 truncate text-[11px] text-neutral-500 dark:text-fg-faint" title={pkg.image}>{pkg.image}</span>
                {/if}
                <span class="image-scan-secondary min-w-0 truncate text-[11px] text-neutral-600 dark:text-fg-dim" title={pkg.licenses.join(', ')}>{pkg.licenses.join(', ')}</span>
              </div>
            {/each}
          </div>
        {/if}
      </div>

      <Pager
        total={filtered.length}
        separator="–"
        storageKey="coolify.page-size.image-scan-packages"
        bind:page={() => shownPage, (p) => (page = p)}
        bind:pageSize={perPage}
      />
    </SettingsSection>
  {/if}
</div>
