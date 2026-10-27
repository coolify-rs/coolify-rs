// Renders stored HTML fragments (`servers.validation_logs`, validation errors)
// that upstream prints with `{!! !!}`. Upstream purifies them before storing;
// this allowlist applies the same rules again in the browser, so rows written
// by anything else cannot inject markup.

const ALLOWED: Record<string, string[]> = {
  a: ['href', 'title', 'target', 'class'],
  br: [],
  div: ['class'],
  pre: ['class'],
  span: ['class'],
  p: ['class'],
};

function clean(node: Node, doc: Document): Node[] {
  if (node.nodeType === Node.TEXT_NODE) return [doc.createTextNode(node.textContent ?? '')];
  if (node.nodeType !== Node.ELEMENT_NODE) return [];
  const el = node as Element;
  const tag = el.tagName.toLowerCase();
  const children = () => Array.from(el.childNodes).flatMap((c) => clean(c, doc));
  if (tag === 'script' || tag === 'style') return [];
  const attrs = ALLOWED[tag];
  if (!attrs) return children();
  const out = doc.createElement(tag);
  for (const name of attrs) {
    const value = el.getAttribute(name);
    if (value === null) continue;
    if (name === 'href' && !/^(https?:|mailto:|\/|#)/i.test(value.trim())) continue;
    if (name === 'target' && value !== '_blank') continue;
    out.setAttribute(name, value);
  }
  if (out.getAttribute('target') === '_blank') out.setAttribute('rel', 'noreferrer noopener');
  out.append(...children());
  return [out];
}

export function sanitizeHtml(html: string | null | undefined): string {
  if (!html) return '';
  const doc = new DOMParser().parseFromString(`<body>${html}</body>`, 'text/html');
  const box = document.createElement('div');
  box.append(...Array.from(doc.body.childNodes).flatMap((c) => clean(c, document)));
  return box.innerHTML;
}
