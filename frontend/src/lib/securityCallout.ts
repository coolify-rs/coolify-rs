// Port of upstream resources/js/security-callout.js and resources/js/vulnerability-search.js.

// The dashboard callout "{n} resources need a security update now." stays dismissed until a
// resource that was not dismissed needs an update now. localStorage (one key per team) keeps the
// dismissed resource UUIDs; the page removes them when no resource needs an update now (`reset`),
// so the next one shows the callout again.
function dismissedSet(stored: string | null): Set<string> | null {
  try {
    const parsed: unknown = JSON.parse(stored ?? 'null');
    return Array.isArray(parsed) ? new Set(parsed.map(String)) : null;
  } catch {
    return null;
  }
}

export function securityCallout(current: string[], stored: string | null): { visible: boolean; reset: boolean } {
  const dismissed = dismissedSet(stored);
  if (current.length === 0) {
    return { visible: false, reset: dismissed !== null };
  }
  return { visible: dismissed === null || current.some((uuid) => !dismissed.has(String(uuid))), reset: false };
}

/** The value to store when the callout is dismissed: the current resources and the ones dismissed before. */
export function dismissSecurityCallout(current: string[], stored: string | null): string {
  return JSON.stringify([...new Set([...(dismissedSet(stored) ?? []), ...current.map(String)])]);
}

/** `Dashboard::securityCalloutTitle()`. */
export function securityCalloutTitle(count: number): string {
  return count === 1 ? '1 resource needs a security update now.' : `${count} resources need a security update now.`;
}

// The command palette entry for a vulnerability ID: "CVE-2021-44228" or "GHSA-jfh8-c2jp-5v3q"
// opens the Security page with that search. `url` is null when the instance does not allow image
// scanning.
const VULNERABILITY_ID = /^(cve|ghsa)-[a-z0-9-]+$/i;

export function vulnerabilitySearchItem(
  query: string | null | undefined,
  url: string | null,
): { name: string; type: string; description: string; link: string; search_text: string } | null {
  const trimmed = String(query ?? '').trim();
  if (!url || !VULNERABILITY_ID.test(trimmed)) {
    return null;
  }
  const id = trimmed.replace(/^[a-z]+/i, (prefix) => prefix.toUpperCase());
  return {
    name: `Find ${id} in your images`,
    type: 'navigation',
    description: 'Search the Security page',
    link: `${url}?search=${encodeURIComponent(id)}`,
    search_text: id,
  };
}
