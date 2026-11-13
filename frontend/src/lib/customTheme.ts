// Port of the custom theme helpers of upstream layouts/base.blade.php. The theme lives in this
// browser's localStorage (keys customTheme, themeColor, customMode); nothing reaches the server.

export type CustomTheme = { sidebar: string; selected: string; accent: string; background: string };
export type ThemePreset = { name: string; theme: CustomTheme | null; swatch?: CustomTheme };

function read(key: string): string | null {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
}

const srgbToLinear = (channel: number) => (channel <= 0.04045 ? channel / 12.92 : ((channel + 0.055) / 1.055) ** 2.4);

const channels = (hex: string) => hex.match(/[a-f\d]{2}/gi)!.map((channel) => parseInt(channel, 16));

/** `window.normalizeThemeColor()` */
export function normalizeThemeColor(color: string | null | undefined): string {
  return strictHex(color) ?? '#6b16ed';
}

function hexToOklab(hex: string | null | undefined): [number, number, number] {
  const [r, g, b] = channels(normalizeThemeColor(hex)).map((channel) => srgbToLinear(channel / 255));
  const l = Math.cbrt(0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b);
  const m = Math.cbrt(0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b);
  const s = Math.cbrt(0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b);
  return [
    0.2104542553 * l + 0.793617785 * m - 0.0040720468 * s,
    1.9779984951 * l - 2.428592205 * m + 0.4505937099 * s,
    0.0259040371 * l + 0.7827717662 * m - 0.808675766 * s,
  ];
}

/** OKLab -> sRGB hex. Out-of-gamut channels are clipped. */
function oklabToHex(okL: number, okA: number, okB: number): string {
  const l = (okL + 0.3963377774 * okA + 0.2158037573 * okB) ** 3;
  const m = (okL - 0.1055613458 * okA - 0.0638541728 * okB) ** 3;
  const s = (okL - 0.0894841775 * okA - 1.291485548 * okB) ** 3;
  return `#${[
    4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s,
    -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s,
    -0.0041960863 * l - 0.7034186147 * m + 1.707614701 * s,
  ]
    .map((c) => {
      const linear = Math.min(1, Math.max(0, c));
      const encoded = linear <= 0.0031308 ? 12.92 * linear : 1.055 * linear ** (1 / 2.4) - 0.055;
      return Math.round(encoded * 255)
        .toString(16)
        .padStart(2, '0');
    })
    .join('')}`;
}

/** `window.hexToOklch()` */
export function hexToOklch(hex: string | null | undefined): string {
  const [okL, okA, okB] = hexToOklab(hex);
  const chroma = Math.sqrt(okA * okA + okB * okB);
  let hue = (Math.atan2(okB, okA) * 180) / Math.PI;
  if (hue < 0) hue += 360;
  return `oklch(${(okL * 100).toFixed(2)}% ${chroma.toFixed(4)} ${hue.toFixed(2)})`;
}

function strictHex(color: string | null | undefined): string | null {
  const hex = String(color ?? '')
    .trim()
    .replace(/^#/, '')
    .toLowerCase();
  if (/^[a-f\d]{6}$/.test(hex)) return `#${hex}`;
  if (/^[a-f\d]{3}$/.test(hex)) return `#${hex.replace(/./g, '$&$&')}`;
  return null;
}

const relativeLuminance = (hex: string) =>
  channels(hex)
    .map((channel) => srgbToLinear(channel / 255))
    .reduce((sum, channel, index) => sum + channel * [0.2126, 0.7152, 0.0722][index], 0);

const mixHex = (from: string, to: string, amount: number) =>
  `#${[1, 3, 5]
    .map((offset) => {
      const start = parseInt(from.slice(offset, offset + 2), 16);
      const end = parseInt(to.slice(offset, offset + 2), 16);
      return Math.round(start + (end - start) * amount)
        .toString(16)
        .padStart(2, '0');
    })
    .join('')}`;

/** `window.themeAccentFill()`: the color of accent fills, the accent with 15% white, like --theme-bright-color in app.css. */
export const themeAccentFill = (color: string | null | undefined) => mixHex(normalizeThemeColor(color), '#ffffff', 0.15);

/** `window.themeAccentForeground()`: black or white, whichever has more contrast on the accent fill (WCAG ratios). */
export function themeAccentForeground(color: string | null | undefined): string {
  const luminance = relativeLuminance(themeAccentFill(color));
  return 1.05 / (luminance + 0.05) > (luminance + 0.05) / 0.05 ? 'oklch(100% 0 0)' : 'oklch(0% 0 0)';
}

/** `window.isDarkColor()`: true when light text has more contrast on this color than dark text. */
export function isDarkColor(hex: string): boolean {
  const luminance = relativeLuminance(normalizeThemeColor(hex));
  return 1.05 / (luminance + 0.05) >= (luminance + 0.05) / (relativeLuminance('#18181b') + 0.05);
}

/** `window.readableTextOn()` */
export const readableTextOn = (hex: string) => (isDarkColor(hex) ? '#f4f4f5' : '#18181b');

const contrast = (first: string, second: string) => {
  const [dark, light] = [relativeLuminance(first), relativeLuminance(second)].sort((a, b) => a - b);
  return (light + 0.05) / (dark + 0.05);
};

/**
 * `window.themeAccentTextOn()`: accent text and icons on a surface. The accent, moved toward white on a dark
 * surface or toward black on a light one until it reaches 4.5:1 (WCAG AA). It is checked on the surface with 12%
 * of its text color, the strongest tint that the surface ladder in app.css puts under text (fills, selected rows).
 */
export function themeAccentTextOn(accent: string, surface: string): string {
  const [okL, okA, okB] = hexToOklab(accent);
  const chromaScale = Math.min(1, 0.16 / (Math.hypot(okA, okB) || 1));
  const target = isDarkColor(surface) ? 1 : 0;
  const tintedSurface = mixHex(normalizeThemeColor(surface), readableTextOn(surface), 0.12);
  let color = '';
  for (let step = 0; step <= 50; step++) {
    const fade = chromaScale * (1 - step / 50);
    color = oklabToHex(okL + ((target - okL) * step) / 50, okA * fade, okB * fade);
    if (contrast(color, tintedSurface) >= 4.5) break;
  }
  return color;
}

const keys = ['sidebar', 'selected', 'accent', 'background'] as const;

/** `window.serializeCustomTheme()`: the share code. */
export const serializeCustomTheme = (theme: CustomTheme) => keys.map((key) => theme[key]).join(',');

/** `window.parseCustomTheme()`: null when the text is not four hex colors. */
export function parseCustomTheme(code: string | null | undefined): CustomTheme | null {
  const colors = String(code ?? '')
    .split(/[\s,]+/)
    .filter(Boolean)
    .map(strictHex);
  if (colors.length !== 4 || colors.includes(null)) return null;
  return { sidebar: colors[0]!, selected: colors[1]!, accent: colors[2]!, background: colors[3]! };
}

/** `window.storedCustomTheme()`: older versions saved one color and a light or dark mode. */
export function storedCustomTheme(): CustomTheme {
  const saved = parseCustomTheme(read('customTheme'));
  if (saved) return saved;
  const accent = normalizeThemeColor(read('themeColor'));
  return read('customMode') === 'light'
    ? { sidebar: mixHex('#f4f4f5', accent, 0.06), selected: mixHex('#ffffff', accent, 0.16), accent, background: mixHex('#fafafa', accent, 0.03) }
    : { sidebar: mixHex('#0c0c0e', accent, 0.1), selected: mixHex('#1c1c1f', accent, 0.3), accent, background: mixHex('#0f0f11', accent, 0.12) };
}

/** `window.customThemePresets`. The first is the built-in Dark theme, not a custom theme. */
export const customThemePresets: ThemePreset[] = [
  { name: 'coolify-rs', theme: null, swatch: { sidebar: '#1a0f2d', selected: '#1c1c1c', accent: '#6b16ed', background: '#0a0a0a' } },
  { name: 'Midnight', theme: { sidebar: '#0f172a', selected: '#2e4a7d', accent: '#f2458f', background: '#1a2338' } },
  { name: 'Forest', theme: { sidebar: '#12342b', selected: '#2c6e57', accent: '#f4b860', background: '#1b2a25' } },
  { name: 'Ember', theme: { sidebar: '#3b1410', selected: '#8f2d1f', accent: '#ffb86b', background: '#241816' } },
  { name: 'Slate', theme: { sidebar: '#2b3038', selected: '#4a5361', accent: '#6cc4f5', background: '#353b45' } },
  { name: 'Nord', theme: { sidebar: '#2e3440', selected: '#4c566a', accent: '#88c0d0', background: '#3b4252' } },
  { name: 'Dracula', theme: { sidebar: '#21222c', selected: '#44475a', accent: '#bd93f9', background: '#282a36' } },
  { name: 'GitHub Dark', theme: { sidebar: '#010409', selected: '#21262d', accent: '#2f81f7', background: '#0d1117' } },
  { name: 'Aubergine', theme: { sidebar: '#3f0e40', selected: '#1164a3', accent: '#1164a3', background: '#ffffff' } },
  { name: 'GitHub Light', theme: { sidebar: '#f6f8fa', selected: '#dde4ec', accent: '#0969da', background: '#ffffff' } },
  { name: 'Paper', theme: { sidebar: '#ebe6da', selected: '#d9d1bf', accent: '#c2410c', background: '#f7f4ec' } },
  { name: 'Blossom', theme: { sidebar: '#fde8f1', selected: '#f9c4dc', accent: '#d61f69', background: '#fff7fa' } },
];

/** `window.applyCustomTheme()`: sets the variables that app.css derives the custom theme from. */
export function applyCustomTheme(theme: CustomTheme): void {
  const root = document.documentElement;
  const isDark = isDarkColor(theme.background);
  const sidebarIsDark = isDarkColor(theme.sidebar);
  Object.entries({
    '--theme-bg': theme.background,
    '--theme-text': readableTextOn(theme.background),
    '--theme-light': isDark ? '0' : '1',
    '--theme-sidebar': theme.sidebar,
    '--theme-sidebar-text': readableTextOn(theme.sidebar),
    '--theme-sidebar-light': sidebarIsDark ? '0' : '1',
    '--theme-selected': theme.selected,
    '--theme-selected-text': readableTextOn(theme.selected),
    '--theme-base-color': hexToOklch(theme.accent),
    '--theme-accent-foreground': themeAccentForeground(theme.accent),
    '--theme-accent-text': themeAccentTextOn(theme.accent, theme.background),
    '--theme-sidebar-accent-text': themeAccentTextOn(theme.accent, theme.sidebar),
  }).forEach(([name, value]) => root.style.setProperty(name, value));
  root.dataset.theme = 'custom';
  root.classList.toggle('dark', isDark);
  if (sidebarIsDark === isDark) delete root.dataset.sidebarScheme;
  else root.dataset.sidebarScheme = sidebarIsDark ? 'dark' : 'light';
  document.getElementById('theme-color-meta')?.setAttribute('content', theme.background);
}
