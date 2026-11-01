// Aligned element geometry of one page on both sides: prints structural differences and elements whose box moved.
//   node full_geom.mjs --rs=18088 [--light] [--limit=30] [--root=selector] [--w=1440] [--h=900] /path [/path...]
import { chromium } from 'playwright-core';
import fs from 'node:fs';

const args = process.argv.slice(2);
const opt = (k, d) => (args.find((a) => a.startsWith(`--${k}=`)) ?? `=${d}`).split('=').slice(1).join('=');
const light = args.includes('--light');
const limit = Number(opt('limit', 30));
const root = opt('root', 'body');
const paths = args.filter((a) => !a.startsWith('--'));
const secret = fs.readFileSync(new URL('../reference/secrets/ui-login.txt', import.meta.url), 'utf8').match(/^password:\s*(.+)$/m)[1].trim();
const browser = await chromium.launch({ executablePath: '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', headless: true });
async function open(base) {
  const ctx = await browser.newContext({ viewport: { width: Number(opt('w', 1440)), height: Number(opt('h', 900)) }, colorScheme: light ? 'light' : 'dark' });
  await ctx.route('**/*', (r) => (new URL(r.request().url()).hostname === '127.0.0.1' ? r.continue() : r.abort()));
  await ctx.addInitScript((l) => { try { localStorage.setItem('theme', l ? 'light' : 'dark'); } catch {} }, light);
  const page = await ctx.newPage();
  await page.goto(base + '/login');
  await page.fill('input[name=email]', 'alice@example.test');
  await page.fill('input[name=password]', secret);
  await Promise.all([page.waitForURL((u) => !u.pathname.startsWith('/login'), { timeout: 20000 }).catch(() => {}), page.click('button[type=submit]')]);
  return page;
}
const pages = [['ref', await open('http://127.0.0.1:18000'), 'http://127.0.0.1:18000'], ['rs', await open(`http://127.0.0.1:${opt('rs', 18088)}`), `http://127.0.0.1:${opt('rs', 18088)}`]];
const dump = (rootSel) => [...document.querySelector(rootSel).querySelectorAll('*')].filter((e) => {
  if (e.closest('svg') && e.tagName !== 'svg') return false;
  if (['SCRIPT', 'STYLE', 'TEMPLATE'].includes(e.tagName)) return false;
  const r = e.getBoundingClientRect();
  return r.width > 0 && r.height > 0 && getComputedStyle(e).visibility !== 'hidden';
}).map((e) => {
  const r = e.getBoundingClientRect();
  const s = getComputedStyle(e);
  const own = [...e.childNodes].filter((n) => n.nodeType === 3).map((n) => n.textContent.trim()).join('').slice(0, 30);
  return { k: `${e.tagName.toLowerCase()}`, c: (e.getAttribute('class') ?? '').trim().split(/\s+/).sort().join(' '), r: [r.x, r.y, r.width, r.height].map((v) => Math.round(v * 10) / 10), t: own, col: s.color, bg: s.backgroundColor, fs: s.fontSize, fw: s.fontWeight };
});
for (const path of paths) {
  const data = {};
  for (const [label, page, base] of pages) {
    await page.goto(base + path);
    await page.waitForLoadState('networkidle', { timeout: 6000 }).catch(() => {});
    await page.waitForTimeout(1500);
    data[label] = await page.evaluate(dump, root);
  }
  const a = data.ref;
  const b = data.rs;
  // Longest common subsequence on tag only keeps moved and restyled elements aligned.
  const key = (x) => `${x.k}|${x.t}`;
  const n = a.length;
  const m = b.length;
  const dp = Array.from({ length: n + 1 }, () => new Uint16Array(m + 1));
  for (let i = n - 1; i >= 0; i--) for (let j = m - 1; j >= 0; j--) dp[i][j] = key(a[i]) === key(b[j]) ? dp[i + 1][j + 1] + 1 : Math.max(dp[i + 1][j], dp[i][j + 1]);
  let i = 0;
  let j = 0;
  const out = [];
  while (i < n || j < m) {
    if (i < n && j < m && key(a[i]) === key(b[j])) {
      const moved = a[i].r.some((v, x) => Math.abs(v - b[j].r[x]) > 0.6);
      const style = a[i].col !== b[j].col || a[i].bg !== b[j].bg || a[i].fs !== b[j].fs || a[i].fw !== b[j].fw;
      if (moved || style || a[i].c !== b[j].c) out.push(`~ ${a[i].k} "${a[i].t}" ${moved ? `box ${a[i].r} -> ${b[j].r}` : ''} ${style ? `style ${a[i].col}/${a[i].bg}/${a[i].fs}/${a[i].fw} -> ${b[j].col}/${b[j].bg}/${b[j].fs}/${b[j].fw}` : ''} ${a[i].c !== b[j].c ? `\n     class ${a[i].c}\n        -> ${b[j].c}` : ''}`);
      i++;
      j++;
    } else if (j >= m || (i < n && dp[i + 1][j] >= dp[i][j + 1])) {
      out.push(`- ${a[i].k} "${a[i].t}" ${a[i].r} .${a[i].c}`);
      i++;
    } else {
      out.push(`+ ${b[j].k} "${b[j].t}" ${b[j].r} .${b[j].c}`);
      j++;
    }
  }
  console.log(`== ${path} (${out.length} differences of ${n}/${m} elements)`);
  console.log(out.slice(0, limit).map((l) => l.slice(0, 420)).join('\n'));
}
await browser.close();
