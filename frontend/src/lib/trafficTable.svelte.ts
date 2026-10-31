// Port of upstream resources/js/traffic-table.js: client-side sort and paging of the traffic lists.
// The rows are rendered in sorted order instead of being moved in the DOM.

export type TableRow<T> = {
  key: string;
  sort: Record<string, number | string | null>;
  /** The "Other" remainder: last in each sort. */
  pinned?: boolean;
  /** Container tooling user agents: left out until `showHidden`. */
  hidable?: boolean;
  data: T;
};

function compareValues(a: unknown, b: unknown): number {
  if (typeof a === 'number' && typeof b === 'number') return a - b;
  return String(a).localeCompare(String(b), undefined, { numeric: true, sensitivity: 'base' });
}

export class TrafficTable<T> {
  pageIndex = $state(0);
  per = 10;
  key = $state('requests');
  dir = $state<'asc' | 'desc'>('desc');
  showHidden = $state(false);
  rows = $state.raw<TableRow<T>[]>([]);

  hiddenCount = $derived(this.rows.filter((row) => row.hidable).length);

  ordered = $derived.by(() => {
    const sign = this.dir === 'asc' ? 1 : -1;
    return this.rows
      .filter((row) => this.showHidden || !row.hidable)
      .map((row, index) => {
        const value = row.sort[this.key] ?? null;
        return { row, index, pinned: row.pinned ? 1 : 0, missing: value === null ? 1 : 0, value };
      })
      .sort((a, b) => a.pinned - b.pinned || a.missing - b.missing || compareValues(a.value, b.value) * sign || a.index - b.index)
      .map(({ row }) => row);
  });

  total = $derived(this.ordered.length);
  page = $derived(Math.min(this.pageIndex, Math.max(0, Math.ceil(this.total / this.per) - 1)));
  visible = $derived(this.ordered.slice(this.page * this.per, (this.page + 1) * this.per));

  /** Text columns start ascending, number columns descending. */
  sortBy(next: string, firstDir: 'asc' | 'desc' = 'desc') {
    if (this.key === next) {
      this.dir = this.dir === 'desc' ? 'asc' : 'desc';
    } else {
      this.key = next;
      this.dir = firstDir;
    }
    this.pageIndex = 0;
  }

  toggleHidden() {
    this.showHidden = !this.showHidden;
    this.pageIndex = 0;
  }

  sorted(column: string): 'asc' | 'desc' | null {
    return this.key === column ? this.dir : null;
  }
}
