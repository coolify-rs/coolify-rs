// The saved theme, applied before the first paint. Upstream runs this inline before <head>
// (layouts/base.blade.php); the UI's Content Security Policy has no inline scripts, so index.html
// loads it as a blocking same-origin script. It sets only what the first paint needs: the theme
// class, the page background of a custom theme and the UI scale. `applyStoredTheme()` of
// src/lib/layout.svelte.ts sets the rest when the app starts.
(() => {
  try {
    const root = document.documentElement;
    const strictHex = (color) => {
      const hex = String(color ?? '').trim().replace(/^#/, '').toLowerCase();
      if (/^[a-f\d]{6}$/.test(hex)) return `#${hex}`;
      if (/^[a-f\d]{3}$/.test(hex)) return `#${hex.replace(/./g, '$&$&')}`;
      return null;
    };
    const srgbToLinear = (channel) => (channel <= 0.04045 ? channel / 12.92 : ((channel + 0.055) / 1.055) ** 2.4);
    const relativeLuminance = (hex) => hex.match(/[a-f\d]{2}/gi)
      .map((channel) => srgbToLinear(parseInt(channel, 16) / 255))
      .reduce((sum, channel, index) => sum + channel * [0.2126, 0.7152, 0.0722][index], 0);
    const mixHex = (from, to, amount) => `#${[1, 3, 5].map((offset) => {
      const start = parseInt(from.slice(offset, offset + 2), 16);
      const end = parseInt(to.slice(offset, offset + 2), 16);
      return Math.round(start + (end - start) * amount).toString(16).padStart(2, '0');
    }).join('')}`;
    // `window.isDarkColor()`
    const isDarkColor = (hex) => {
      const luminance = relativeLuminance(hex);
      return 1.05 / (luminance + 0.05) >= (luminance + 0.05) / (relativeLuminance('#18181b') + 0.05);
    };
    // The background of `window.storedCustomTheme()`.
    const customBackground = () => {
      const colors = String(localStorage.customTheme ?? '').split(/[\s,]+/).filter(Boolean).map(strictHex);
      if (colors.length === 4 && !colors.includes(null)) return colors[3];
      const accent = strictHex(localStorage.themeColor) ?? '#6b16ed';
      return localStorage.customMode === 'light' ? mixHex('#fafafa', accent, 0.03) : mixHex('#0f0f11', accent, 0.12);
    };

    // `window.storedUiScale()`
    root.dataset.uiScale = ['80', '90', '100'].includes(localStorage.uiScale) ? localStorage.uiScale : '100';
    root.dataset.navStyle = localStorage.navStyle === 'floating' ? 'floating' : 'default';
    root.dataset.navItems = localStorage.navItems === 'flat' ? 'flat' : '3d';
    const theme = localStorage.theme === 'purple' ? 'custom' : (localStorage.theme || 'dark');
    if (theme === 'custom') {
      const background = customBackground();
      root.style.setProperty('--theme-bg', background);
      root.dataset.theme = 'custom';
      root.classList.toggle('dark', isDarkColor(background));
      return;
    }
    const isDark = theme === 'dark' || (theme === 'system' && matchMedia('(prefers-color-scheme: dark)').matches);
    root.classList.toggle('dark', isDark);
    root.dataset.theme = isDark ? 'dark' : 'light';
  } catch {
    // Storage can be unavailable (private mode): the page keeps the default dark theme.
  }
})();
