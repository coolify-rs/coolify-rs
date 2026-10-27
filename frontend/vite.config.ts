import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';
import tailwindcss from '@tailwindcss/vite';

// In development, API and session calls go to a running coolify-rs server.
const backend = process.env.COOLIFY_RS_BACKEND ?? 'http://127.0.0.1:18080';

export default defineConfig({
  plugins: [tailwindcss(), svelte()],
  server: {
    port: 5173,
    proxy: {
      '/ui': backend,
      '/api': backend,
    },
  },
  build: {
    outDir: 'dist',
    assetsDir: 'assets',
    sourcemap: false,
  },
});
