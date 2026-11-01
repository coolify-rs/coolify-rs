// Every page from a manifest on both sides, in one colour mode, with what triage needs next to the screenshots.
//   node full_pass.mjs --rs=18088 [--light] [--conc=4] [--w=1440] [--h=900] [--prefix=dark] manifest.json
// Manifest: [{ name, path, rsPath?, guest? }]. Output: shots/<prefix>-<name>-{ref,rs}.png and shots/<prefix>-<name>.json
// (final url, title, favicon, visible text, class outline of each side). Compare with full_triage.py.
// Requests to anything but 127.0.0.1 are aborted. The password is read from the fixture and never printed.
import { chromium } from 'playwright-core';
import fs from 'node:fs';

const args = process.argv.slice(2);
const opt = (k, d) => (args.find((a) => a.startsWith(`--${k}=`)) ?? `=${d}`).split('=')[1];
const flag = (k) => args.includes(`--${k}`);
const light = flag('light');
const rs = opt('rs', 18088);
const conc = Number(opt('conc', 4));
const prefix = opt('prefix', light ? 'light' : 'dark');
const width = Number(opt('w', 1440));
const height = Number(opt('h', 900));
const targets = JSON.parse(fs.readFileSync(args.find((a) => !a.startsWith('--')), 'utf8'));
const secret = fs.readFileSync(new URL('../reference/secrets/ui-login.txt', import.meta.url), 'utf8').match(/^password:\s*(.+)$/m)[1].trim();
const out = new URL('./shots/', import.meta.url).pathname;
fs.mkdirSync(out, { recursive: true });
const browser = await chromium.launch({ executablePath: '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', headless: true });

async function context(storageState) {
  const ctx = await browser.newContext({ viewport: { width, height }, colorScheme: light ? 'light' : 'dark', storageState });
  await ctx.route('**/*', (r) => (new URL(r.request().url()).hostname === '127.0.0.1' ? r.continue() : r.abort()));
  await ctx.addInitScript((l) => { try { localStorage.setItem('theme', l ? 'light' : 'dark'); } catch {} }, light);
  return ctx;
}

async function login(base) {
  const ctx = await context();
  const page = await ctx.newPage();
  await page.goto(base + '/login');
  await page.fill('input[name=email]', 'alice@example.test');
  await page.fill('input[name=password]', secret);
  await Promise.all([page.waitForURL((u) => !u.pathname.startsWith('/login'), { timeout: 20000 }).catch(() => {}), page.click('button[type=submit]')]);
  const state = await ctx.storageState();
  await ctx.close();
  return state;
}

const collect = () => {
  const vis = (e) => { const r = e.getBoundingClientRect(); const s = getComputedStyle(e); return r.width > 0 && r.height > 0 && s.visibility !== 'hidden' && s.display !== 'none'; };
  const icon = [...document.querySelectorAll('link[rel~=icon]')].map((l) => l.getAttribute('href'));
  const main = document.querySelector('main') ?? document.body;
  const outline = [...main.querySelectorAll('*')].filter(vis).filter((e) => !e.closest('svg') || e.tagName === 'svg').map((e) => `${e.tagName.toLowerCase()}.${(e.getAttribute('class') ?? '').trim().split(/\s+/).sort().join('.')}`);
  const controls = [...document.querySelectorAll('input,select,textarea,button,a[href]')].filter(vis).map((e) => `${e.tagName.toLowerCase()}|${e.getAttribute('type') ?? ''}|${e.getAttribute('placeholder') ?? ''}|${(e.innerText ?? '').trim().slice(0, 50)}`);
  return { title: document.title, icon, text: document.body.innerText, outline, controls, htmlClass: document.documentElement.className };
};

const side = {
  ref: { base: 'http://127.0.0.1:18000' },
  rs: { base: `http://127.0.0.1:${rs}` },
};
for (const s of Object.values(side)) {
  s.state = await login(s.base);
  s.user = await context(s.state);
  s.guest = await context();
}

async function shoot(t, label) {
  const s = side[label];
  const ctx = t.guest ? s.guest : s.user;
  const page = await ctx.newPage();
  const result = { status: 0 };
  try {
    const res = await page.goto(s.base + (label === 'rs' ? t.rsPath ?? t.path : t.path), { timeout: 45000, waitUntil: 'domcontentloaded' });
    result.status = res?.status() ?? 0;
    await page.waitForLoadState('networkidle', { timeout: 8000 }).catch(() => {});
    await page.waitForTimeout(1800);
    for (let i = 0; i < 2; i++) {
      const popup = page.locator('button:has-text("Accept And Close"), button:has-text("Accept and Close"), button:has-text("Maybe next time")').filter({ visible: true }).first();
      if (await popup.isVisible().catch(() => false)) await popup.click({ timeout: 2000 }).catch(() => {});
    }
    await page.waitForTimeout(150);
    result.url = new URL(page.url()).pathname;
    Object.assign(result, await page.evaluate(collect));
    // A very long page (the new resource list) is too tall to capture: fall back to the viewport.
    await page.screenshot({ path: `${out}${prefix}-${t.name}-${label}.png`, fullPage: true }).catch(() => page.screenshot({ path: `${out}${prefix}-${t.name}-${label}.png` }));
  } catch (e) {
    result.error = String(e.message).split('\n')[0].slice(0, 200);
  }
  await page.close();
  return result;
}

let next = 0;
let done = 0;
await Promise.all(Array.from({ length: conc }, async () => {
  while (next < targets.length) {
    const t = targets[next++];
    const [ref, rsr] = await Promise.all([shoot(t, 'ref'), shoot(t, 'rs')]);
    fs.writeFileSync(`${out}${prefix}-${t.name}.json`, JSON.stringify({ target: t, ref, rs: rsr }));
    if (++done % 25 === 0) console.log(`${prefix} ${done}/${targets.length}`);
  }
}));
await browser.close();
console.log(`${prefix} done ${done}/${targets.length}`);
