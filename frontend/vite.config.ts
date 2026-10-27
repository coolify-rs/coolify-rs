import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';
import tailwindcss from '@tailwindcss/vite';
import fs from 'node:fs';
import path from 'node:path';
import type { Plugin } from 'vite';

// In development, API and session calls go to a running coolify-rs server.
const backend = process.env.COOLIFY_RS_BACKEND ?? 'http://127.0.0.1:18080';

// Upstream serves Monaco's AMD build from /js/monaco-editor-{version}/min
// (components/forms/monaco-editor.blade.php loads it on demand); so does coolify-rs.
const monacoVersion = JSON.parse(fs.readFileSync('node_modules/monaco-editor/package.json', 'utf8')).version;
const monacoSource = path.resolve('node_modules/monaco-editor/min');
const monacoPrefix = `/js/monaco-editor-${monacoVersion}/min/`;
function monaco(): Plugin {
  return {
    name: 'coolify-monaco',
    configureServer(server) {
      server.middlewares.use((req, res, next) => {
        if (!req.url?.startsWith(monacoPrefix)) return next();
        const file = path.join(monacoSource, decodeURIComponent(req.url.slice(monacoPrefix.length).split('?')[0]));
        if (!file.startsWith(monacoSource) || !fs.existsSync(file)) return next();
        res.setHeader('Content-Type', file.endsWith('.css') ? 'text/css' : 'application/javascript');
        fs.createReadStream(file).pipe(res);
      });
    },
    closeBundle() {
      fs.cpSync(path.join(monacoSource, 'vs'), path.resolve('dist', monacoPrefix.slice(1), 'vs'), { recursive: true });
    },
  };
}

export default defineConfig({
  plugins: [tailwindcss(), svelte(), monaco()],
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
