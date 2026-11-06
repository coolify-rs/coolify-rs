<script lang="ts">
  // coolify-rs's own (ADR 0013): one plugin panel. The panel is a document of the plugin, served by
  // `/ui/plugins/{id}/panels/{slot}` with a sandboxing policy and shown in a frame with
  // `sandbox="allow-scripts"` and nothing else, so it runs in an opaque origin: no cookies, no access to
  // this page, no network. Its only way out is the message bridge below, which makes requests to the
  // plugin's own routes (`/ui/ext/{id}/...`) for it, after checking every message.
  import { csrf } from '../lib/api';
  import type { PanelRef } from '../lib/plugins.svelte';

  let { panel, resource = null }: { panel: PanelRef; resource?: string | null } = $props();

  /** Limits of the bridge: request and answer sizes, requests in flight, frame height. */
  const MAX_BODY = 64 * 1024;
  const MAX_ANSWER = 1024 * 1024;
  const MAX_IN_FLIGHT = 8;
  const MIN_HEIGHT = 48;
  const MAX_HEIGHT = 4000;
  const METHODS = ['GET', 'POST', 'PUT', 'PATCH', 'DELETE'];

  let frame: HTMLIFrameElement | undefined = $state();
  let height = $state(160);
  let closed = $state(false);
  let loads = 0;
  let inFlight = 0;

  const isDark = () => document.documentElement.classList.contains('dark');
  // Read once: the frame is not reloaded when the theme changes, the bridge tells it instead.
  // svelte-ignore state_referenced_locally
  const src = `/ui/plugins/${panel.plugin}/panels/${panel.slot}?panel=${panel.index}${resource ? `&resource=${encodeURIComponent(resource)}` : ''}&theme=${isDark() ? 'dark' : 'light'}`;

  function post(message: Record<string, unknown>) {
    // The frame's origin is opaque ("null"), so `*` is the only target; the window itself is ours.
    if (!closed) frame?.contentWindow?.postMessage(message, '*');
  }

  function sendTheme() {
    post({ type: 'coolify-rs:theme', theme: isDark() ? 'dark' : 'light' });
  }

  /** A path of the plugin's own routes: no dot segments (also encoded), no `//`, no backslashes. */
  function validPath(p: unknown): p is string {
    if (typeof p !== 'string' || p.length > 1024 || !p.startsWith('/')) return false;
    if (p.includes('//') || p.includes('\\') || /%2e|%5c/i.test(p) || p.includes('#')) return false;
    const [pathname] = p.split('?', 1);
    if (pathname.split('/').some((s) => s === '.' || s === '..')) return false;
    return /^[A-Za-z0-9\-._~!$&'()*+,;=:@%/?]*$/.test(p);
  }

  async function bridge(m: { id?: unknown; method?: unknown; path?: unknown; body?: unknown }) {
    const id = m.id;
    if (typeof id !== 'number' || !Number.isSafeInteger(id)) return;
    const fail = (error: string) => post({ type: 'coolify-rs:response', id, error });
    const method = typeof m.method === 'string' ? m.method.toUpperCase() : '';
    if (!METHODS.includes(method)) return fail('Unsupported method.');
    if (!validPath(m.path)) return fail('Invalid path.');
    const body = m.body ?? null;
    if (body !== null && (typeof body !== 'string' || body.length > MAX_BODY)) return fail('The request body must be a string of at most 64 KiB.');
    if (body !== null && method === 'GET') return fail('GET requests have no body.');
    if (inFlight >= MAX_IN_FLIGHT) return fail('Too many requests at once.');
    inFlight++;
    try {
      const headers: Record<string, string> = { Accept: 'application/json' };
      if (method !== 'GET') headers['X-CSRF-Token'] = csrf();
      if (body !== null) headers['Content-Type'] = 'application/json';
      const res = await fetch(`/ui/ext/${panel.plugin}${m.path}`, {
        method,
        headers,
        body,
        credentials: 'same-origin',
        redirect: 'error',
        cache: 'no-store',
      });
      const text = await res.text();
      if (text.length > MAX_ANSWER) return fail('The answer is larger than 1 MiB.');
      post({ type: 'coolify-rs:response', id, status: res.status, contentType: res.headers.get('content-type') ?? '', body: text });
    } catch {
      fail('The request failed.');
    } finally {
      inFlight--;
    }
  }

  function onMessage(e: MessageEvent) {
    if (closed || !frame || e.source !== frame.contentWindow) return;
    const m = e.data;
    if (!m || typeof m !== 'object') return;
    if (m.type === 'coolify-rs:ready') sendTheme();
    else if (m.type === 'coolify-rs:resize' && Number.isFinite(m.height)) height = Math.min(MAX_HEIGHT, Math.max(MIN_HEIGHT, Math.ceil(m.height)));
    else if (m.type === 'coolify-rs:request') bridge(m);
  }

  // A second load means the panel navigated its own frame away from its document: close it.
  function onLoad() {
    loads++;
    if (loads > 1) closed = true;
    else sendTheme();
  }

  $effect(() => {
    const observer = new MutationObserver(sendTheme);
    observer.observe(document.documentElement, { attributes: true, attributeFilter: ['class'] });
    return () => observer.disconnect();
  });
</script>

<svelte:window onmessage={onMessage} />

{#if closed}
  <p class="text-[12px] text-neutral-500 dark:text-fg-dim">This panel was closed because it tried to leave its frame.</p>
{:else}
  <iframe
    bind:this={frame}
    {src}
    title="{panel.title} ({panel.name})"
    sandbox="allow-scripts"
    referrerpolicy="no-referrer"
    loading="lazy"
    class="block w-full border-0 bg-transparent"
    style="height: {height}px"
    onload={onLoad}
  ></iframe>
{/if}
