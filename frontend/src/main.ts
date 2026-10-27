import { mount } from 'svelte';
import './app.css';
import App from './App.svelte';

// Theme: saved choice, else the system preference (dark by default).
try {
  const saved = localStorage.getItem('theme');
  const dark = saved ? saved === 'dark' : !window.matchMedia('(prefers-color-scheme: light)').matches;
  document.documentElement.classList.toggle('dark', dark);
} catch {
  // Storage can be unavailable; keep the default dark theme.
}

const target = document.getElementById('app');
if (target) mount(App, { target });
