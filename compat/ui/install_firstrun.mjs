// First-run screens of two clean installs, through to the first user and onboarding, in dark mode.
//   node install_firstrun.mjs <upstream port> <coolify-rs port> <password file>
// The ports are tunnels from compat/diff/install/orb_tunnel.py. The password file gets a new random test
// password that is used on both throwaway installs and is never printed.
// Output: compat/ui/shots/fr-<step>-{ref,rs}.png and the text of each screen compared.
import { chromium } from 'playwright-core';
import fs from 'node:fs';
import crypto from 'node:crypto';

const [upPort, rsPort, passFile] = process.argv.slice(2);
const password = `Fr-${crypto.randomBytes(12).toString('base64url')}-9a!`;
fs.writeFileSync(passFile, password, { mode: 0o600 });
const email = 'first-root@example.test';
const out = new URL('./shots/', import.meta.url).pathname;
fs.mkdirSync(out, { recursive: true });
const browser = await chromium.launch({
  executablePath: '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
  headless: true,
});

const clean = (t) =>
  t
    .replace(/coolify-rs/gi, 'Coolify')
    .replace(/\s+/g, ' ')
    .trim();

async function side(label, port) {
  const base = `http://127.0.0.1:${port}`;
  const ctx = await browser.newContext({ viewport: { width: 1440, height: 900 }, colorScheme: 'dark' });
  await ctx.addInitScript(() => {
    try {
      localStorage.setItem('theme', 'dark');
    } catch {}
  });
  const page = await ctx.newPage();
  const seen = {};
  const shot = async (name) => {
    await page.waitForTimeout(1800);
    await page.screenshot({ path: `${out}fr-${name}-${label}.png` });
    seen[name] = { path: new URL(page.url()).pathname, text: clean(await page.locator('body').innerText()) };
  };
  const step = async (name, action) => {
    try {
      await action();
    } catch (e) {
      seen[`${name}-failed`] = { path: new URL(page.url()).pathname, text: String(e.message).split('\n')[0] };
    }
    await shot(name);
  };

  await page.goto(`${base}/`);
  await shot('root');
  await page.goto(`${base}/login`);
  await shot('login');
  await page.goto(`${base}/register`);
  await shot('register');
  await step('register-errors', async () => {
    await page.fill('input[name=name]', 'First Root');
    await page.fill('input[name=email]', email);
    await page.fill('input[name=password]', 'short');
    await page.fill('input[name=password_confirmation]', 'different');
    await page.click('button[type=submit]');
  });
  await step('after-register', async () => {
    await page.fill('input[name=name]', 'First Root');
    await page.fill('input[name=email]', email);
    await page.fill('input[name=password]', password);
    await page.fill('input[name=password_confirmation]', password);
    await page.click('button[type=submit]');
    await page.waitForURL((u) => !u.pathname.startsWith('/register'), { timeout: 20000 });
  });
  await page.goto(`${base}/`);
  await shot('home');
  await page.goto(`${base}/onboarding`);
  await shot('welcome');
  await step('server-type', async () => {
    await page.getByRole('button', { name: 'Continue', exact: true }).first().click();
  });
  await step('localhost', async () => {
    await page.getByRole('button', { name: /This Machine/i }).first().click();
  });
  await page.goto(`${base}/settings`);
  await shot('settings');
  const guest = await browser.newContext({ viewport: { width: 1440, height: 900 }, colorScheme: 'dark' });
  const g = await guest.newPage();
  await g.goto(`${base}/register`);
  await g.waitForTimeout(1800);
  seen['register-after-root'] = { path: new URL(g.url()).pathname, text: clean(await g.locator('body').innerText()) };
  await g.screenshot({ path: `${out}fr-register-after-root-${label}.png` });
  await guest.close();
  await ctx.close();
  return seen;
}

const ref = await side('ref', upPort);
const rs = await side('rs', rsPort);
await browser.close();
let same = 0;
const names = [...new Set([...Object.keys(ref), ...Object.keys(rs)])];
for (const name of names) {
  const a = ref[name];
  const b = rs[name];
  if (a && b && a.path === b.path && a.text === b.text) {
    same++;
    console.log(`same  ${name} ${a.path}`);
  } else {
    console.log(`DIFF  ${name}\n  ref ${a?.path}: ${a?.text?.slice(0, 500)}\n  rs  ${b?.path}: ${b?.text?.slice(0, 500)}`);
  }
}
console.log(`${same}/${names.length} screens with the same path and text`);
