/** Moves the element to `<body>`, like Alpine's `x-teleport="body"`: page-level
 *  CSS (for example rules scoped to `.app-main`) then no longer applies to it. */
export function portal(node: HTMLElement) {
  document.body.appendChild(node);
  return { destroy: () => node.remove() };
}
