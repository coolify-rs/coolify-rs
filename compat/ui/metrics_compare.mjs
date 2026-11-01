// Compares the metrics and analytics screens of the reference and coolify-rs on the rows and the VM of
// compat/diff/metrics_diff.py: visible text and the tag/class sequence per section, with a screenshot pair.
//   python3 compat/diff/metrics_diff.py --keep --sentinel=fake     then
//   cd compat/ui && node metrics_compare.mjs --rs=18087 --set=fake
// Sets: fake (charts, frozen data), real (traffic analytics), enable (the empty fleet page and its button).
import { execFileSync } from 'node:child_process';
import { chromium } from 'playwright-core';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

const argv = process.argv.slice(2);
const opt = (k, d) => (argv.find((a) => a.startsWith(`--${k}=`)) ?? `=${d}`).split('=').slice(1).join('=');
const rsPort = opt('rs', 18087);
const set = opt('set', 'fake');
const only = opt('only', '');
const scheme = argv.includes('--light') ? 'light' : 'dark';
const state = JSON.parse(fs.readFileSync(path.join(process.env.TMPDIR ?? os.tmpdir(), 'coolify-metrics-diff.json'), 'utf8'));
const secret = fs.readFileSync(new URL('../reference/secrets/ui-login.txt', import.meta.url), 'utf8').match(/^password:\s*(.+)$/m)[1].trim();
const out = new URL('./shots/', import.meta.url).pathname;
fs.mkdirSync(out, { recursive: true });
const BASE = { ref: 'http://127.0.0.1:18000', rs: `http://127.0.0.1:${rsPort}` };
const browser = await chromium.launch({ executablePath: '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', headless: true });
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const DIFF = new URL('../diff/metrics_diff.py', import.meta.url).pathname;
const VM = new URL('../diff/metrics_vm.sh', import.meta.url).pathname;
const diff = (flag) => execFileSync('python3', [DIFF, flag], { encoding: 'utf8' });

async function session(base, user) {
  const ctx = await browser.newContext({ viewport: { width: 1440, height: 900 }, colorScheme: scheme, timezoneId: 'UTC', locale: 'en-US' });
  const page = await ctx.newPage();
  await page.addInitScript((light) => {
    try {
      localStorage.setItem('theme', light ? 'light' : 'dark');
    } catch {
      // No storage on this document.
    }
  }, scheme === 'light');
  await page.goto(`${base}/login`);
  await page.fill('input[name=email]', `${user}@example.test`);
  await page.fill('input[name=password]', secret);
  await Promise.all([page.waitForURL((u) => !u.pathname.startsWith('/login'), { timeout: 20000 }).catch(() => {}), page.click('button[type=submit]')]);
  return page;
}

let failed = 0;
let passed = 0;
function verdict(name, ok, detail) {
  if (ok) passed++;
  else failed++;
  console.log(`${ok ? 'PASS' : 'FAIL'} ${name}${ok ? '' : `\n     ${String(detail).replaceAll(secret, '***').slice(0, 900)}`}`);
}

const capture = (page) =>
  page.evaluate(() => {
    const visible = (el) => {
      const s = getComputedStyle(el);
      return s.display !== 'none' && (s.visibility !== 'hidden' || el.tagName === 'IMG');
    };
    const signature = (root) => {
      const list = [];
      const walk = (el) => {
        if (!visible(el) || ['TEMPLATE', 'SCRIPT', 'STYLE'].includes(el.tagName)) return;
        const cls = [...new Set((el.getAttribute('class') ?? '').split(/\s+/))].filter((c) => c && !/^apexcharts[a-z0-9]{6,}$/.test(c)).sort().join(' ');
        if (cls || !['DIV', 'SPAN'].includes(el.tagName)) list.push(`${el.tagName.toLowerCase()}.${cls}`);
        // ApexCharts and the globe draw their own content.
        if (el.tagName === 'svg' || el.tagName === 'CANVAS' || el.classList.contains('apexcharts-canvas')) return;
        for (const child of el.children) walk(child);
      };
      walk(root);
      return list;
    };
    const sections = [...document.querySelectorAll('section')].filter(visible).map((section) => ({
      title: section.dataset.settingsSectionTitle ?? section.querySelector('h2,h3')?.innerText.trim() ?? section.id,
      // A chart's axis ticks follow the second of the read; its points are compared by metrics_diff.py.
      text: (() => {
        const hidden = [...section.querySelectorAll('.apexcharts-canvas')];
        hidden.forEach((chart) => (chart.style.display = 'none'));
        const text = section.innerText.replace(/[ \t]+/g, ' ').trim();
        hidden.forEach((chart) => (chart.style.display = ''));
        return text;
      })(),
      sig: signature(section),
      charts: section.querySelectorAll('.apexcharts-canvas').length,
    }));
    const empties = [...document.querySelectorAll('.empty-state')].filter((e) => !e.closest('section')).map((e) => e.innerText.replace(/\s+/g, ' ').trim());
    const heading = (document.querySelector('h1')?.innerText ?? '').trim();
    return { sections, empties, heading, search: location.search };
  });

const removePopups = (page) =>
  page
    .evaluate(() => {
      for (const d of document.querySelectorAll('div')) {
        if (getComputedStyle(d).position === 'fixed' && /Cannot connect to real-time service|Love Coolify|No notifications enabled/.test(d.innerText ?? '')) d.remove();
      }
    })
    .catch(() => {});

async function settle(page, wait) {
  await page.waitForFunction(() => !document.querySelector('.animate-pulse'), null, { timeout: 30000 }).catch(() => {});
  await sleep(wait);
  await removePopups(page);
}

function firstDifference(a, b) {
  for (let i = 0; i < Math.max(a.length, b.length); i++) {
    if (a[i] !== b[i]) return `#${i}\n     ref ${a[i]}\n     rs  ${b[i]}`;
  }
  return '';
}

async function screen(pages, name, ref, rsPath = ref, { wait = 2500, before, sections, hiddenCharts = false } = {}) {
  if (only && !name.includes(only)) return;
  // Read inside one minute: the stand-in's data moves on the full minute.
  if (new Date().getSeconds() > 30) await sleep((61 - new Date().getSeconds()) * 1000);
  const shots = {};
  for (const side of ['ref', 'rs']) {
    // The reference's scheduler can move these rows' status at any moment.
    diff('--touch');
    const page = pages[side];
    await page.goto(BASE[side] + (side === 'ref' ? ref : rsPath));
    await page.waitForLoadState('networkidle', { timeout: 3000 }).catch(() => {});
    if (before) await before(page, side);
    await settle(page, wait);
    shots[side] = await capture(page);
    await page.screenshot({ path: `${out}${name}-${side}.png`, fullPage: true });
  }
  const { ref: a, rs: b } = shots;
  if (sections) {
    a.sections = a.sections.filter((s) => sections.includes(s.title));
    b.sections = b.sections.filter((s) => sections.includes(s.title));
  }
  verdict(`${name}: heading, empties`, JSON.stringify([a.heading, a.empties]) === JSON.stringify([b.heading, b.empties]), `ref ${JSON.stringify([a.heading, a.empties])}\n     rs  ${JSON.stringify([b.heading, b.empties])}`);
  verdict(`${name}: sections`, JSON.stringify(a.sections.map((s) => s.title)) === JSON.stringify(b.sections.map((s) => s.title)), `ref ${a.sections.map((s) => s.title)}\n     rs  ${b.sections.map((s) => s.title)}`);
  a.sections.forEach((x, i) => {
    const y = b.sections[i];
    if (!y || x.title !== y.title) return;
    verdict(`${name} [${x.title}]: text`, x.text === y.text, firstDifference(x.text.split('\n'), y.text.split('\n')));
    verdict(`${name} [${x.title}]: markup`, x.sig.join('|') === y.sig.join('|'), firstDifference(x.sig, y.sig));
    // Upstream keeps the hidden grid in the page while the table shows, with its charts.
    if (!hiddenCharts) verdict(`${name} [${x.title}]: charts`, x.charts === y.charts, `ref ${x.charts} rs ${y.charts}`);
  });
}

const P = `/project/${state.project}/environment/${state.environment}`;
const S = state.servers;
const APP = 'metricsapp1aaaaaaaaaaaaaa';
const DB = 'metricsdb1aaaaaaaaaaaaaaa';
const SVC = 'metricssvc1aaaaaaaaaaaaaa';

if (set === 'fake' || set === 'real') {
  const pages = { ref: await session(BASE.ref, 'metricsbot'), rs: await session(BASE.rs, 'metricsbot') };
  await removePopups(pages.ref);
  if (set === 'fake') {
    await screen(pages, 'm-fleet', '/metrics', '/metrics', { wait: 3500 });
    await screen(pages, 'm-fleet-busy', `/metrics?server=${S.busy}`, `/metrics?server=${S.busy}`, { wait: 3500 });
    await screen(pages, 'm-fleet-old', `/metrics?server=${S.old}`, `/metrics?server=${S.old}`, { wait: 3500 });
    await screen(pages, 'm-fleet-bad', `/metrics?server=${S.badtoken}`, `/metrics?server=${S.badtoken}`, { wait: 3500 });
    await screen(pages, 'm-fleet-7d', '/metrics?range=7d', '/metrics?range=7d', { wait: 3500 });
    // The Projects section belongs to the canvas stream (its Canvas column and modal are not here yet).
    const servers = ['Servers', 'Traffic analytics'];
    const view = (mode, title) => async (page, side) => {
      if (side === 'ref') {
        // Upstream's toggle is Alpine state that a click did not reliably reach on the dashboard; the view is saved per user.
        await page.waitForFunction(() => window.Alpine, null, { timeout: 15000 });
        await page.evaluate((m) => {
          for (const el of document.querySelectorAll('[x-data]')) window.Alpine.$data(el).setViewMode?.(m);
        }, mode);
        await sleep(1500);
        return;
      }
      for (let i = 0; i < 2; i++) {
        const button = page.locator(`button[title="${title}"]:visible`).first();
        if (await button.count()) await button.click();
        await sleep(600);
      }
    };
    const grid = view('grid', 'Grid view');
    const table = view('table', 'Table view');
    await screen(pages, 'm-dashboard', '/', '/', { wait: 3500, before: table, sections: servers, hiddenCharts: true });
    await screen(pages, 'm-dashboard-grid', '/', '/', { wait: 3500, before: grid, sections: servers });
    await screen(pages, 'm-servers-grid', '/servers', '/servers', { wait: 3500, before: grid });
    await screen(pages, 'm-servers', '/servers', '/servers', { wait: 3500, before: table, hiddenCharts: true });
    await screen(pages, 'm-dashboard-table', '/', '/', { wait: 3500, before: table, sections: servers, hiddenCharts: true });
    await screen(pages, 'm-server-charts', `/server/${S.busy}/metrics`);
    await screen(pages, 'm-app-metrics', `${P}/application/${APP}/metrics`);
    await screen(pages, 'm-app-compose', `${P}/application/metricsappcompose00000000/metrics`);
    await screen(pages, 'm-app-stopped', `${P}/application/metricsappstopped00000000/metrics`);
    await screen(pages, 'm-app-off', `${P}/application/metricsappmetricsoff00000/metrics`);
    await screen(pages, 'm-db-metrics', `${P}/database/${DB}/metrics`);
    await screen(pages, 'm-app-analytics-off', `${P}/application/metricsappmetricsoff00000/analytics`);
    await screen(pages, 'm-svc-analytics-off', `${P}/service/metricssvcoff000000000000/analytics`);
    // The stand-in has no traffic routes: every read fails.
    await screen(pages, 'm-app-analytics-nodata', `${P}/application/${APP}/analytics`, `${P}/application/${APP}/analytics`, { wait: 4000 });
    // The widget's lazy placeholder has no box, so it stays empty on both sides, also with a visible activity column.
    await screen(pages, 'm-dashboard-traffic-idle', '/', '/', { wait: 8000, before: table, sections: ['Servers', 'Traffic analytics'], hiddenCharts: true });
    diff('--deployment-on');
    try {
      await screen(pages, 'm-dashboard-traffic-active', '/', '/', { wait: 12000, before: table, sections: ['Servers', 'Traffic analytics'], hiddenCharts: true });
    } finally {
      diff('--deployment-off');
    }
  } else {
    await screen(pages, 'm-app-analytics', `${P}/application/${APP}/analytics`, `${P}/application/${APP}/analytics`, { wait: 4000 });
    await screen(pages, 'm-svc-analytics', `${P}/service/${SVC}/analytics`, `${P}/service/${SVC}/analytics`, { wait: 4000 });
  }
}

if (set === 'enable') {
  const pages = { ref: await session(BASE.ref, 'metricsbot2'), rs: await session(BASE.rs, 'metricsbot2') };
  await screen(pages, 'm-fleet-enable', '/metrics');
  // `enableMetrics` on each side: the toast, the page after it, the Sentinel it started and the audit row.
  const inspect = () => execFileSync(VM, ['run'], { input: "docker inspect coolify-sentinel --format '{{range .Config.Env}}{{println .}}{{end}}' | grep -E '^(COLLECTOR_ENABLED|PUSH_ENDPOINT|COLLECTOR_REFRESH|COLLECTOR_RETENTION|TRAFFIC_ENABLED)' | sort", encoding: 'utf8' });
  const results = {};
  for (const side of ['ref', 'rs']) {
    const page = pages[side];
    await page.goto(`${BASE[side]}/metrics`);
    await settle(page, 2000);
    await page.locator('button:has-text("Enable metrics on")').click();
    const toast = await page.getByText('Metrics enabled on 1 server. Sentinel is starting, data shows in about one minute.').first().waitFor({ timeout: 60000 }).then(() => true, () => false);
    await sleep(20000);
    await page.goto(`${BASE[side]}/metrics`);
    await settle(page, 3000);
    results[side] = { toast, overview: await page.locator('#metrics-overview-section').count(), env: inspect(), audit: JSON.parse(diff('--audit').trim() || 'null') };
    diff('--metrics-off');
  }
  const clean = (r) => JSON.stringify(r);
  verdict('enable metrics: toast, page, Sentinel env, audit row', clean(results.ref) === clean(results.rs), `ref ${clean(results.ref)}\n     rs  ${clean(results.rs)}`);
  console.log(results.rs.env);
}

await browser.close();
console.log(`${passed}/${passed + failed} passed`);
process.exit(failed ? 1 : 0);
