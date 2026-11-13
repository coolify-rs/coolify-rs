<script lang="ts">
  // The frame of upstream server pages (livewire/server/navbar.blade.php below
  // xl + the settings workspace with the in-page copy of the server menu,
  // which upstream's CSS hides because the main sidebar shows it).
  import type { Snippet } from 'svelte';
  import { page } from '../lib/page.svelte';
  import ServerProxyActions from './ServerProxyActions.svelte';
  import ServerTopbar from './ServerTopbar.svelte';
  import { showProxyActions } from '../lib/serverPage.svelte';

  let { name, children }: { name: string; children: Snippet } = $props();
</script>

<div>
  <nav class="w-full max-w-none pb-3 lg:pb-0">
    <div>
      <div class="mb-3 flex w-full min-w-0 items-start gap-3 xl:hidden">
        <div class="min-w-0 flex-1 lg:hidden">
          <div class="flex min-w-0 flex-col gap-2">
            <h1 data-testid="server-subtitle" class="min-w-0 truncate text-[24px]! leading-7! font-semibold! tracking-tight! text-black dark:text-fg">
              {name}
            </h1>
            {#if page.server}
              <div class="flex w-full min-w-0 items-center gap-2">
                <ServerTopbar server={page.server} statusOnly />
              </div>
            {/if}
          </div>
        </div>
        <div class="ml-auto shrink-0">
          {#if page.server?.proxySet && showProxyActions(page.server.uuid)}
            <ServerProxyActions server={page.server} mobile />
          {/if}
        </div>
      </div>
    </div>
  </nav>
  <div
    class="server-settings-workspace application-settings-workspace mt-4 grid w-full max-w-none min-w-0 gap-8 lg:mt-0 xl:grid-cols-[210px_minmax(0,1fr)] xl:gap-8"
  >
    <aside class="application-settings-navigation in-main-nav min-w-0"></aside>
    <div class="w-full min-w-0">
      {@render children()}
    </div>
  </div>
</div>
