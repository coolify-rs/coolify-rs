/** PHP `strnatcmp` (Laravel `sortBy(..., SORT_NATURAL)`): digit runs compare
 *  by value, everything else by byte, so case matters ('B' < 'a'). */
export function strnatcmp(a: string, b: string): number {
  const re = /(\d+|\D+)/g;
  const xa = a.match(re) ?? [];
  const xb = b.match(re) ?? [];
  for (let i = 0; i < Math.min(xa.length, xb.length); i++) {
    const pa = xa[i];
    const pb = xb[i];
    if (pa === pb) continue;
    const na = /^\d/.test(pa);
    const nb = /^\d/.test(pb);
    if (na && nb) {
      const d = Number(pa) - Number(pb);
      if (d !== 0) return d < 0 ? -1 : 1;
      continue;
    }
    return pa < pb ? -1 : 1;
  }
  return xa.length - xb.length;
}

/** `SORT_NATURAL | SORT_FLAG_CASE` (PHP `strnatcasecmp`): natural order, case-insensitive. */
export function strnatcasecmp(a: string, b: string): number {
  return strnatcmp(a.toLowerCase(), b.toLowerCase());
}

export function sortNatural<T>(items: T[], key: (t: T) => string): T[] {
  return [...items].sort((a, b) => strnatcmp(key(a), key(b)));
}
