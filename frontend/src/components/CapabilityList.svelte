<script lang="ts">
  // coolify-rs's own (ADR 0010, ADR 0013): a plugin's capabilities, each with what it lets the plugin do.
  let { capabilities }: { capabilities: string[] } = $props();

  function explain(c: string): string {
    const [name, rest] = c.split(/:\s*/, 2);
    switch (name) {
      case 'events':
        return `Receives these events for teams that enable it: ${rest}.`;
      case 'storage':
        return 'Keeps its own data for each team that enables it, up to 10 MiB per team.';
      case 'routes':
        return 'Answers API requests under its own prefix, for teams that enable it.';
      case 'http':
        return `Sends HTTPS requests to these hosts, and no others: ${rest}.`;
      case 'ui':
        return `Shows panels in the web interface: ${rest}.`;
      default:
        return c;
    }
  }
</script>

{#if capabilities.length}
  <ul class="space-y-1.5">
    {#each capabilities as c (c)}
      <li class="flex items-start gap-2 text-[12px] leading-5">
        <code class="shrink-0 rounded-md bg-neutral-100 px-1.5 font-mono text-[11px] text-neutral-700 dark:bg-white/[0.06] dark:text-fg-dim">{c.split(':', 1)[0]}</code>
        <span class="min-w-0 text-neutral-600 dark:text-fg-dim">{explain(c)}</span>
      </li>
    {/each}
  </ul>
{:else}
  <p class="text-[12px] text-neutral-500 dark:text-fg-dim">No capabilities: the plugin can only write to the log.</p>
{/if}
