// The Alpine state of the session toolbar, the same in terminal/index.blade.php and
// execute-container-command.blade.php: theme and mode of the console.

export const consoleThemes = [
  { key: 'system', name: 'System', background: 'linear-gradient(135deg, #ffffff 0 50%, #121214 50% 100%)', accent: '#8C8E9C' },
  { key: 'shadows-midnight', name: 'Midnight', background: 'linear-gradient(135deg, #2a3b4c, rgba(42, 59, 76, 0.4))', accent: '#6d7a7c' },
  { key: 'shadows-golden-hour', name: 'Golden Hour', background: 'linear-gradient(135deg, #d58a42, rgba(213, 138, 66, 0.4))', accent: '#bf8c3c' },
  { key: 'shadows-cosmic-purple', name: 'Cosmic Purple', background: 'linear-gradient(135deg, #5d3e66, rgba(93, 62, 102, 0.4))', accent: '#A76DBE' },
  { key: 'shadows-neon-glow', name: 'Neon Glow', background: 'linear-gradient(135deg, #f300a6, rgba(243, 0, 166, 0.3))', accent: '#DB425A' },
  { key: 'shadows-icy-mist', name: 'Icy Mist', background: 'linear-gradient(135deg, #d0d8e2, rgba(208, 216, 226, 0.2))', accent: '#93b7c4' },
  { key: 'shadows-tropical-storm', name: 'Tropical Storm', background: 'linear-gradient(135deg, #00b894, #1fa771, #2ecc71, #27ae60)', accent: '#1fa771' },
  { key: 'shadows-golden-nebula', name: 'Golden Nebula', background: 'linear-gradient(135deg, #ffd700, #ff6347, #d4a20e, #ffcc00, #1f3d6f)', accent: '#d4a20e' },
  { key: 'shadows-cosmic-lagoon', name: 'Cosmic Lagoon', background: 'linear-gradient(135deg, #1d2b64, #2f4f96, #00b5b8, #9c27b0, #8e24aa)', accent: '#00b5b8' },
  { key: 'shadows-neon-nebula', name: 'Neon Nebula', background: 'linear-gradient(135deg, #00d9d9, #ff55aa, #1e1e2f, #2f3b57, #ff99ff)', accent: '#ff55aa' },
  { key: 'shadows-transparent', name: 'Blur Black', background: 'rgba(0, 0, 0, 0.7)', accent: '#8C8E9C' },
];

const THEME_KEY = 'coolify-console-theme';

function savedTheme(): string {
  const saved = localStorage.getItem(THEME_KEY);
  return consoleThemes.some((t) => t.key === saved) ? (saved as string) : 'system';
}

export class ConsoleChrome {
  consoleTheme = $state('system');
  themeOpen = $state(false);
  attachAvailable = $state(false);
  hasShell = $state(true);
  terminalMode = $state<'shell' | 'attach'>('shell');
  modeOpen = $state(false);
  accent = $derived(consoleThemes.find((t) => t.key === this.consoleTheme)?.accent ?? '#8C8E9C');
  themeName = $derived(consoleThemes.find((t) => t.key === this.consoleTheme)?.name ?? 'System');

  /** Upstream's `Livewire.dispatch('set-terminal-mode', { mode })`. */
  onModeChange: (mode: 'shell' | 'attach') => void = () => {};

  /** `init()`. */
  init(): void {
    this.consoleTheme = savedTheme();
    localStorage.setItem(THEME_KEY, this.consoleTheme);
  }

  setTheme(theme: string): void {
    this.consoleTheme = theme;
    this.themeOpen = false;
    localStorage.setItem(THEME_KEY, theme);
    window.dispatchEvent(new CustomEvent('terminal-theme-change', { detail: { theme } }));
  }

  setTerminalMode(mode: 'shell' | 'attach'): void {
    this.modeOpen = false;
    if (this.terminalMode === mode || (mode === 'shell' && !this.hasShell)) return;
    this.terminalMode = mode;
    window.dispatchEvent(new CustomEvent('terminal-starting'));
    this.onModeChange(mode);
  }

  syncTheme(): void {
    this.consoleTheme = savedTheme();
  }

  /** The `.window` listeners of the section; returns what removes them. */
  listen(): () => void {
    const starting = () => this.syncTheme();
    const updated = (e: Event) => {
      const d = (e as CustomEvent).detail;
      this.attachAvailable = d.attachAvailable;
      this.terminalMode = d.mode;
      this.hasShell = d.hasShell;
    };
    window.addEventListener('terminal-starting', starting);
    window.addEventListener('terminal-mode-updated', updated);
    return () => {
      window.removeEventListener('terminal-starting', starting);
      window.removeEventListener('terminal-mode-updated', updated);
    };
  }
}
