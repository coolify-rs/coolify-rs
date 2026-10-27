const MONTHS = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];

/** PHP `format('M j, Y')` of a `YYYY-MM-DD ...` / ISO timestamp, read as UTC. */
export function monthDayYear(ts: string | null | undefined): string {
  const m = ts?.match(/^(\d{4})-(\d{2})-(\d{2})/);
  if (!m) return '-';
  return `${MONTHS[Number(m[2]) - 1]} ${Number(m[3])}, ${m[1]}`;
}
