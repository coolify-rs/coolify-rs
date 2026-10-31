// Runs the same UI steps on the reference and on coolify-rs and diffs what happened: toasts, rows
// and audit_events. Both sides share one Postgres, so a flow restores its rows between the sides.
//   import { flow, psql, step } from './flow.mjs';
// Node scripts next to this file call flow(rsPort, user, async (ctx) => {...}) with one ctx per side.
import { chromium } from 'playwright-core';
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';

const secret = fs.readFileSync(new URL('../reference/secrets/ui-login.txt', import.meta.url), 'utf8').match(/^password:\s*(.+)$/m)[1].trim();
export const tokens = JSON.parse(fs.readFileSync(new URL('../reference/secrets/tokens.json', import.meta.url), 'utf8'));
const out = new URL('./shots/', import.meta.url).pathname;
fs.mkdirSync(out, { recursive: true });

export function psql(q) {
  return execFileSync('docker', ['exec', 'coolify-rs-reference-postgres-1', 'psql', '-U', 'coolify', '-d', 'coolify', '-tAc', q], { encoding: 'utf8' }).trim();
}

/** Every SQL statement a flow needs to put a table row back exactly. */
export function saveRow(table, where) {
  const j = psql(`select row_to_json(t) from ${table} t where ${where}`);
  return { table, where, row: j ? JSON.parse(j) : null };
}
export function restoreRow(s) {
  if (!s.row) {
    psql(`delete from ${s.table} where ${s.where}`);
    return;
  }
  const cols = Object.keys(s.row);
  const lit = (v) => (v === null ? 'NULL' : `'${String(typeof v === 'object' ? JSON.stringify(v) : v).replace(/'/g, "''")}'`);
  psql(`update ${s.table} set ${cols.map((c) => `"${c}" = ${lit(s.row[c])}`).join(', ')} where ${s.where}`);
}

export const auditMax = () => Number(psql('select coalesce(max(id),0) from audit_events') || 0);
export function auditSince(id, event = '.') {
  const cols = 'team_id,event,source,action,level,actor_type,actor_id,actor_name,actor_email,actor_token_id,actor_token_name,resource_type,resource_name,description,metadata,(changes is not null) as has_changes';
  const t = psql(`select coalesce(json_agg(r), '[]') from (select ${cols} from audit_events where id > ${id} and event ~ '${event}' order by id) r`);
  return JSON.parse(t);
}

async function login(browser, base, user) {
  const ctx = await browser.newContext({ viewport: { width: 1440, height: 900 }, colorScheme: 'dark', permissions: ['clipboard-read', 'clipboard-write'] });
  const page = await ctx.newPage();
  await page.addInitScript(() => {
    try {
      localStorage.setItem('theme', 'dark');
    } catch {}
    // Collects every toast that appears.
    window.__toasts = [];
    const seen = new WeakSet();
    const scan = () => {
      for (const el of document.querySelectorAll('.toast-card')) {
        if (seen.has(el)) continue;
        seen.add(el);
        setTimeout(() => window.__toasts.push(el.innerText.replace(/\s+/g, ' ').trim()), 100);
      }
    };
    document.addEventListener('DOMContentLoaded', () => new MutationObserver(scan).observe(document.documentElement, { childList: true, subtree: true }));
  });
  await page.goto(base + '/login');
  await page.fill('input[name=email]', `${user}@example.test`);
  await page.fill('input[name=password]', secret);
  await Promise.all([page.waitForURL((u) => !u.pathname.startsWith('/login'), { timeout: 15000 }).catch(() => {}), page.click('button[type=submit]')]);
  return page;
}

function helpers(page, base, label) {
  return {
    label,
    page,
    base,
    async go(path) {
      await page.goto(base + path);
      await page.waitForTimeout(1500);
      for (const t of ['Accept And Close', 'Accept and Close', 'Maybe next time']) {
        await page.locator(`button:has-text("${t}")`).first().click({ timeout: 300 }).catch(() => {});
      }
      await page.evaluate(() => (window.__toasts = []));
    },
    /** Switches the session team through the breadcrumb switcher. */
    async switchTeam(current, target) {
      try {
        await page.locator('button:visible', { hasText: current }).first().click({ timeout: 3000 });
      } catch {
        return;
      }
      await page.waitForTimeout(300);
      await page.locator('button:visible', { hasText: target }).first().click();
      await page.waitForTimeout(1500);
    },
    /** Toast texts since the last call. */
    async toasts(wait = 1500) {
      await page.waitForTimeout(wait);
      return page.evaluate(() => window.__toasts.splice(0));
    },
    async shot(name, full = true) {
      await page.screenshot({ path: `${out}${name}-${label}.png`, fullPage: full });
    },
    button: (name) => page.getByRole('button', { name, exact: true }).first(),
    /** Picks an option of a labelled upstream listbox. */
    async listbox(fieldLabel, option) {
      await page.locator('label:visible', { hasText: fieldLabel }).first().locator('xpath=ancestor::div[.//button[contains(@class,"listbox-trigger")]][1]//button[contains(@class,"listbox-trigger")]').first().click();
      await page.waitForTimeout(150);
      await page.locator('.listbox-option:visible', { hasText: option }).first().click();
    },
    /** Toggles an option of an event multiselect, then closes it. */
    async event(triggerId, option) {
      await page.locator(`#${triggerId}-trigger`).click();
      await page.waitForTimeout(150);
      await page.locator('.listbox-option:visible', { hasText: option }).first().click();
      await page.waitForTimeout(300);
      await page.keyboard.press('Escape');
    },
    /** The input next to a label; upstream's labels do not always point at their input. */
    field(fieldLabel) {
      return page.locator('label:visible', { hasText: fieldLabel }).first().locator('xpath=ancestor::div[.//input][1]//input').first();
    },
    async enterIn(fieldLabel) {
      await this.field(fieldLabel).press('Enter');
    },
  };
}

/** Compares two JSON-able values and prints the first differences. Returns true when equal. */
export function diff(name, a, b) {
  const norm = (v) => JSON.stringify(v).replace(/127\.0\.0\.1:\d+/g, 'HOST');
  const ja = norm(a);
  const jb = norm(b);
  if (ja === jb) {
    console.log(`ok   ${name}`);
    return true;
  }
  // Only the part that differs, to keep the output small.
  let i = 0;
  while (i < ja.length && ja[i] === jb[i]) i++;
  const from = Math.max(0, i - 120);
  console.log(`DIFF ${name}\n  ref ...${ja.slice(from, i + 240)}\n  rs  ...${jb.slice(from, i + 240)}`);
  process.exitCode = 1;
  return false;
}

/** Runs `body(h)` on each side; `body` returns a record of observations; prints their diff. */
export async function flow(port, user, body, opts = {}) {
  const browser = await chromium.launch({ executablePath: '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', headless: true });
  const results = {};
  try {
    for (const [label, base] of [
      ['ref', 'http://127.0.0.1:18000'],
      ['rs', `http://127.0.0.1:${port}`],
    ]) {
      opts.before?.();
      const page = await login(browser, base, user);
      const h = helpers(page, base, label);
      try { results[label] = await body(h); } finally { opts.after?.(); }
      await page.context().close();
    }
  } finally {
    await browser.close();
  }
  for (const k of Object.keys(results.ref)) diff(k, results.ref[k], results.rs[k]);
  return results;
}
