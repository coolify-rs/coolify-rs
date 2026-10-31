// Remembered table/grid choice per list (upstream keeps it per user via
// `saveListView`; coolify-rs keeps it in the browser).

export function listView(key: string, fallback = 'table') {
  const storageKey = `listView:${key}`;
  let initial = fallback;
  try {
    // `?view=` (the canvas page's link back) wins and is remembered, as in `listView()` upstream.
    const asked = new URLSearchParams(window.location.search).get('view');
    if (asked === 'table' || asked === 'grid') localStorage.setItem(storageKey, asked);
    initial = localStorage.getItem(storageKey) ?? fallback;
  } catch {
    // Storage unavailable: use the default view.
  }
  let mode = $state(initial);
  return {
    get mode() {
      return mode;
    },
    set mode(v: string) {
      mode = v;
      try {
        localStorage.setItem(storageKey, v);
      } catch {
        // Storage unavailable: the choice lasts for this page only.
      }
    },
  };
}
