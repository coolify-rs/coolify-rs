// Root setup states of coolify-rs on an empty database, against upstream's Blade rendered by tinker.
//   node first_user.mjs open|closed <directory with first-open.html, first-closed.html>
import { chromium } from 'playwright-core';
import fs from 'node:fs';

const [step, dir] = process.argv.slice(2);
const out = new URL('./shots/', import.meta.url).pathname;
const RS = 'http://127.0.0.1:18086';
const browser = await chromium.launch({ executablePath: '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', headless: true });
for (const light of [false, true]) {
  const suffix = light ? '-l' : '';
  const ctx = await browser.newContext({ viewport: { width: 1440, height: 900 }, colorScheme: light ? 'light' : 'dark' });
  await ctx.addInitScript((l) => { try { localStorage.setItem('theme', l ? 'light' : 'dark'); } catch {} }, light);
  // The reference's page, from the file, with its own assets.
  const ref = await ctx.newPage();
  await ref.route('http://127.0.0.1:18000/c-batch-first', (r) => r.fulfill({ contentType: 'text/html', body: fs.readFileSync(`${dir}/first-${step}.html`, 'utf8') }));
  await ref.goto('http://127.0.0.1:18000/c-batch-first');
  await ref.waitForTimeout(1200);
  await ref.screenshot({ path: `${out}c-first-${step}${suffix}-ref.png` });
  const page = await ctx.newPage();
  await page.goto(`${RS}/login`);
  await page.waitForTimeout(1200);
  console.log(step, 'login goes to', new URL(page.url()).pathname);
  await page.screenshot({ path: `${out}c-first-${step}${suffix}-rs.png` });
  if (step === 'closed' && !light) {
    const res = await page.evaluate(() => fetch('/ui/auth/register', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ name: 'x', email: 'c-batch-root@example.test', password: 'C-batch-Pass1!', password_confirmation: 'C-batch-Pass1!' }) }).then((r) => r.json()));
    console.log('closed window error:', res.errors?.email?.[0]);
  }
  if (step === 'open' && light) {
    await page.fill('input[name=name]', 'C Batch Root');
    await page.fill('input[name=email]', 'c-batch-root@example.test');
    await page.fill('input[name=password]', 'C-batch-Pass1!');
    await page.fill('input[name=password_confirmation]', 'C-batch-Pass1!');
    await page.click('button[type=submit]');
    await page.waitForTimeout(2000);
    console.log('after the root account:', new URL(page.url()).pathname);
    await page.goto(`${RS}/register`);
    await page.waitForTimeout(1000);
    console.log('registration afterwards (signed in):', new URL(page.url()).pathname);
    const guest = await browser.newContext();
    const g = await guest.newPage();
    await g.goto(`${RS}/register`);
    await g.waitForTimeout(1000);
    console.log('registration afterwards (guest):', new URL(g.url()).pathname);
    await guest.close();
  }
  await ctx.close();
}
await browser.close();
