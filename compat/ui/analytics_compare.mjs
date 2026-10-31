// Compares the `livewire:analytics` views of the reference and coolify-rs on the same servers and the
// same Sentinel data: visible text and the tag/class sequence per section, per state and interaction.
//   node analytics_compare.mjs --rs=18087 --a=<server uuid> --b=<server uuid> --app=<application uuid on a>
import { chromium } from 'playwright-core';
import fs from 'node:fs';

const argv = process.argv.slice(2);
const opt = (k, d) => (argv.find((a) => a.startsWith(`--${k}=`)) ?? `=${d}`).split('=').slice(1).join('=');
const rsPort = opt('rs', 18087);
const A = opt('a', '');
const B = opt('b', '');
const APP = opt('app', '');
const scheme = argv.includes('--light') ? 'light' : 'dark';
const secret = fs.readFileSync(new URL('../reference/secrets/ui-login.txt', import.meta.url), 'utf8').match(/^password:\s*(.+)$/m)[1].trim();
const out = new URL('./shots/analytics/', import.meta.url).pathname;
fs.mkdirSync(out, { recursive: true });
const BASE = { ref: 'http://127.0.0.1:18000', rs: `http://127.0.0.1:${rsPort}` };
const browser = await chromium.launch({ executablePath: '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', headless: true });
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function session(base) {
  const ctx = await browser.newContext({ viewport: { width: 1440, height: 900 }, colorScheme: scheme });
  const page = await ctx.newPage();
  await page.addInitScript((light) => {
    try {
      localStorage.setItem('theme', light ? 'light' : 'dark');
    } catch {
      // No storage on this document.
    }
  }, scheme === 'light');
  await page.goto(`${base}/login`);
  await page.fill('input[name=email]', 'alice@example.test');
  await page.fill('input[name=password]', secret);
  await Promise.all([page.waitForURL((u) => !u.pathname.startsWith('/login'), { timeout: 15000 }).catch(() => {}), page.click('button[type=submit]')]);
  return page;
}
const pages = { ref: await session(BASE.ref), rs: await session(BASE.rs) };

let failed = 0;
let passed = 0;
function verdict(name, ok, detail) {
  if (ok) passed++;
  else failed++;
  console.log(`${ok ? 'PASS' : 'FAIL'} ${name}${ok ? '' : `\n     ${detail}`}`);
}

const capture = (page) =>
  page.evaluate(() => {
    const visible = (el) => {
      const s = getComputedStyle(el);
      // A flag or favicon that failed to load is hidden; whether it failed depends on when it loaded.
      return s.display !== 'none' && (s.visibility !== 'hidden' || el.tagName === 'IMG');
    };
    const signature = (root) => {
      const list = [];
      const walk = (el) => {
        if (!visible(el) || el.tagName === 'TEMPLATE' || el.tagName === 'SCRIPT' || el.tagName === 'STYLE') return;
        const cls = (el.getAttribute('class') ?? '').split(/\s+/).filter((c) => c && !/^apexcharts[a-z0-9]{6,}$/.test(c)).sort().join(' ');
        if (cls || !['DIV', 'SPAN'].includes(el.tagName)) list.push(`${el.tagName.toLowerCase()}.${cls}`);
        // ApexCharts and the globe draw their own content.
        if (el.tagName === 'svg' || el.tagName === 'CANVAS' || el.classList.contains('apexcharts-canvas')) return;
        for (const child of el.children) walk(child);
      };
      walk(root);
      return list;
    };
    const sections = {};
    for (const section of document.querySelectorAll('section[id^="analytics-"]')) {
      sections[section.id] = {
        text: section.innerText.replace(/[ \t]+/g, ' ').trim(),
        sig: signature(section),
        charts: [...section.querySelectorAll('.apexcharts-canvas')].length,
        canvas: [...section.querySelectorAll('canvas')].length,
        rect: Math.round(section.getBoundingClientRect().height),
      };
    }
    // The filter bar and the empty states outside the sections.
    const heading = (document.querySelector('h1')?.innerText ?? '').trim();
    const bar = ['serverUuid', 'appUuid', 'range'].map((id) => document.getElementById(`${id}-trigger`)?.innerText.trim() ?? null);
    const empties = [...document.querySelectorAll('.empty-state')].filter((e) => !e.closest('section')).map((e) => e.innerText.replace(/\s+/g, ' ').trim());
    return { sections, order: Object.keys(sections), heading, bar, empties, search: location.search };
  });

const removePopups = (page) =>
  page
    .evaluate(() => {
      for (const d of document.querySelectorAll('div')) {
        if (getComputedStyle(d).position === 'fixed' && /Cannot connect to real-time service|Love Coolify|No notifications enabled/.test(d.innerText ?? '')) d.remove();
      }
    })
    .catch(() => {});

async function settle(page) {
  await page.waitForFunction(() => !document.querySelector('.animate-pulse'), null, { timeout: 30000 }).catch(() => {});
  await sleep(1800);
  await removePopups(page);
}
async function open(side, path) {
  await pages[side].goto(BASE[side] + path);
  await pages[side].waitForLoadState('networkidle', { timeout: 8000 }).catch(() => {});
  await settle(pages[side]);
}

function compare(name, a, b) {
  verdict(`${name}: heading and filters`, JSON.stringify([a.heading, a.bar, a.empties]) === JSON.stringify([b.heading, b.bar, b.empties]), `ref ${JSON.stringify([a.heading, a.bar, a.empties])}\n     rs  ${JSON.stringify([b.heading, b.bar, b.empties])}`);
  verdict(`${name}: section order`, JSON.stringify(a.order) === JSON.stringify(b.order), `ref ${a.order}\n     rs  ${b.order}`);
  for (const id of a.order) {
    const x = a.sections[id];
    const y = b.sections[id];
    if (!y) continue;
    verdict(`${name}: ${id} text`, x.text === y.text, firstDifference(x.text.split('\n'), y.text.split('\n')));
    verdict(`${name}: ${id} markup`, JSON.stringify(x.sig) === JSON.stringify(y.sig), firstDifference(x.sig, y.sig));
    verdict(`${name}: ${id} charts`, x.charts === y.charts && x.canvas === y.canvas && x.rect === y.rect, `charts ${x.charts}/${y.charts} canvas ${x.canvas}/${y.canvas} height ${x.rect}/${y.rect}`);
  }
}
function firstDifference(x, y) {
  for (let i = 0; i < Math.max(x.length, y.length); i++) {
    if (x[i] !== y[i]) return `#${i}\n     ref ${x[i]}\n     rs  ${y[i]}`;
  }
  return '';
}

async function state(name, path) {
  await Promise.all(['ref', 'rs'].map((side) => open(side, path)));
  await both(name);
}
async function both(name) {
  const [a, b] = await Promise.all([capture(pages.ref), capture(pages.rs)]);
  compare(name, a, b);
  for (const side of ['ref', 'rs']) await pages[side].screenshot({ path: `${out}${name.replace(/\W+/g, '_')}-${side}.png`, fullPage: true });
}
async function act(name, fn) {
  const results = await Promise.allSettled(['ref', 'rs'].map(async (side) => {
    await fn(pages[side]);
    await settle(pages[side]);
  }));
  const errors = results.filter((r) => r.status === 'rejected').map((r) => String(r.reason).slice(0, 200));
  verdict(`${name}: ran`, errors.length === 0, errors.join(' | '));
  await both(name);
}
async function chooseFilter(page, id, label) {
  await page.locator(`[id="${id}-trigger"]`).click();
  await sleep(300);
  await page.locator(`[id="${id}-panel"] button[role=option]`).filter({ hasText: label }).first().click();
  await sleep(300);
}
const panelText = async (page, id) => {
  await page.locator(`[id="${id}-trigger"]`).click();
  await sleep(400);
  const text = await page.locator(`[id="${id}-panel"]`).innerText();
  await page.keyboard.press('Escape');
  return text.replace(/\s+/g, ' ').trim();
};

const S = `/server/${A}/analytics`;
await state('scoped', S);
{
  const texts = await Promise.all(['ref', 'rs'].map((side) => panelText(pages[side], 'appUuid')));
  verdict('scoped: application filter options', texts[0] === texts[1], `ref ${texts[0]}\n     rs  ${texts[1]}`);
}
await act('sort paths by path', async (p) => p.locator('#analytics-paths-section button[title="Sort by path"]').click());
await act('sort paths by path again', async (p) => p.locator('#analytics-paths-section button[title="Sort by path"]').click());
await act('paths next page', async (p) => p.locator('#analytics-paths-section button', { hasText: 'Next' }).click());
await act('show container tooling', async (p) => p.locator('#analytics-useragent-section button', { hasText: 'container tooling' }).click());
await act('cities', async (p) => p.locator('#analytics-country-section button', { hasText: 'Cities' }).click());
await act('countries', async (p) => p.locator('#analytics-country-section button', { hasText: 'Countries' }).click());
await act('range 7d', async (p) => chooseFilter(p, 'range', 'Last 7 days'));
{
  const urls = await Promise.all(['ref', 'rs'].map((side) => pages[side].evaluate(() => location.search)));
  verdict('range 7d: in the address', urls.every((u) => u.includes('range=7d')), JSON.stringify(urls));
}
await act('range 30d', async (p) => chooseFilter(p, 'range', 'Last 30 days'));
await act('range 24h', async (p) => chooseFilter(p, 'range', 'Last 24 hours'));
await act('live on', async (p) => p.locator('button[aria-pressed]', { hasText: 'Paused' }).click());
await act('live off', async (p) => p.locator('button[aria-pressed]', { hasText: 'Live' }).click());

await state('global', '/analytics');
{
  const texts = await Promise.all(['ref', 'rs'].map((side) => panelText(pages[side], 'serverUuid')));
  verdict('global: server filter options', texts[0] === texts[1], `ref ${texts[0]}\n     rs  ${texts[1]}`);
}
await act('global server filter', async (p) => chooseFilter(p, 'serverUuid', 'diff-ui-a'));
await act('global application filter', async (p) => chooseFilter(p, 'appUuid', 'diff-traffic-a'));
await state('global bookmarked server', `/analytics?server=${B}`);
await state('global bookmarked application', `/analytics?app=${APP}`);
await state('global bookmarked 7d', '/analytics?range=7d');
await state('global unknown server filter', '/analytics?server=nope');
await state('global unknown application filter', '/analytics?app=nope');
await state('scoped second server', `/server/${B}/analytics`);

await browser.close();
console.log(`${passed}/${passed + failed} checks passed`);
process.exit(failed ? 1 : 0);
