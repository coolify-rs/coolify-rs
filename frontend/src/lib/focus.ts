/** Focuses an element when it mounts (`autofocus` does not fire for elements
 *  Svelte inserts after the initial page load). */
export function focusOnMount(node: HTMLElement) {
  queueMicrotask(() => node.focus());
}
