import { mount } from 'svelte';
import './styles.css';
import App from './App.svelte';
import { applyStoredTheme } from './lib/layout.svelte';

applyStoredTheme();

const target = document.getElementById('app');
if (target) mount(App, { target });
