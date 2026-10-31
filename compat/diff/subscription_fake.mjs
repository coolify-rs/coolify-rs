// The Coolify Cloud path of coolify-rs against compat/diff/stripe_fake.py. The reference runs
// self-hosted, so this checks the Stripe calls and rows against upstream's PHP by hand, not by diff.
//   python3 compat/diff/stripe_fake.py 18192 &
//   SELF_HOSTED=false SUBSCRIPTION_PROVIDER=stripe STRIPE_API_BASE=http://127.0.0.1:18192 STRIPE_API_KEY=sk_fake STRIPE_WEBHOOK_SECRET=whsec_fake \
//     STRIPE_PRICE_ID_DYNAMIC_MONTHLY=price_m STRIPE_PRICE_ID_DYNAMIC_YEARLY=price_y scripts/dev-server.sh 18092
//   COOLIFY_RS_BASE=http://127.0.0.1:18092 node compat/diff/subscription_fake.mjs
// Works on a team it creates, which has no servers, and deletes that team and its subscription by id.
// The login password is read from the fixture file and never printed.
import { createRequire } from 'node:module';
import { createHmac } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';

const ROOT = new URL('../', import.meta.url);
const require = createRequire(new URL('ui/package.json', ROOT));
const { chromium } = require('playwright-core');
const RS = process.env.COOLIFY_RS_BASE ?? 'http://127.0.0.1:18092';
const FAKE = process.env.STRIPE_FAKE ?? 'http://127.0.0.1:18192';
const secret = fs.readFileSync(new URL('reference/secrets/ui-login.txt', ROOT), 'utf8').match(/^password:\s*(.*)$/m)[1].trim();
const sql = (q) => execFileSync('docker', ['exec', 'coolify-rs-reference-postgres-1', 'psql', '-U', 'coolify', '-d', 'coolify', '-Atc', q], { encoding: 'utf8' }).trim();
const shots = new URL('ui/shots/', ROOT).pathname;

let pass = 0;
const fails = [];
const check = (name, ok, detail) => {
  if (ok) pass++;
  else fails.push(`${name}: ${JSON.stringify(detail)?.slice(0, 400)}`);
};
const fake = async (path) => (await fetch(FAKE + path)).json();

const browser = await chromium.launch({ executablePath: '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', headless: true });
const ctx = await browser.newContext({ colorScheme: 'dark', viewport: { width: 1440, height: 900 } });
const page = await ctx.newPage();
await page.addInitScript(() => localStorage.setItem('theme', 'dark'));
await page.goto(`${RS}/login`);
await page.fill('input[name=email]', 'alice@example.test');
await page.fill('input[name=password]', secret);
await Promise.all([page.waitForURL((u) => !u.pathname.startsWith('/login'), { timeout: 15000 }), page.keyboard.press('Enter')]);

const call = (method, path, body) =>
  page.evaluate(
    async ({ method, path, body }) => {
      const session = await (await fetch('/ui/session')).json();
      const r = await fetch(path, { method, headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': session.csrf_token, Accept: 'application/json' }, body: body === undefined ? undefined : JSON.stringify(body) });
      const text = await r.text();
      let json = null;
      try {
        json = JSON.parse(text);
      } catch {}
      return { status: r.status, json, text: json ? undefined : text.slice(0, 300) };
    },
    { method, path, body },
  );

const before = (await call('GET', '/ui/session')).json.current_team.id;
let team = null;
try {
  const made = await call('POST', '/ui/team/new', { name: `sub-fake-${Date.now() % 100000}`, description: '' });
  team = made.json.team_id;
  check('team created', made.status < 300 && team, made);
  await call('POST', '/ui/session/team', { team_id: team });

  let d = await call('GET', '/ui/subscription');
  check('cloud page', d.json.cloud === true && d.json.has_subscription === false && d.json.server_limits === 2, d.json);

  // Index with no subscription.
  await page.goto(`${RS}/subscription`);
  await page.waitForTimeout(1000);
  check('show without subscription goes to index', new URL(page.url()).pathname === '/subscription/new', page.url());
  await page.screenshot({ path: `${shots}sub-index-rs.png` });

  // Checkout.
  let r = await call('POST', '/ui/subscription/subscribe', { type: 'dynamic-yearly' });
  check('checkout url', r.json.url?.includes('/checkout/'), r.json);
  let log = await fake('/__log');
  const customer = log.findLast((l) => l.path === '/v1/customers');
  check('customer idempotency', customer?.idempotency === `coolify-team-${team}-customer` && customer.params['metadata[team_id]'] === String(team), customer);
  const session = log.findLast((l) => l.path === '/v1/checkout/sessions' && l.method === 'POST');
  check('checkout parameters', session?.params['line_items[0][price]'] === 'price_y' && session.params['line_items[0][adjustable_quantity][minimum]'] === '2' && session.params['subscription_data[metadata][team_id]'] === String(team) && session.params.mode === 'subscription' && session.params['client_reference_id']?.endsWith(`:${team}`), session?.params);
  const row = () => JSON.parse(sql(`SELECT row_to_json(s) FROM subscriptions s WHERE team_id = ${team}`) || 'null');
  check('customer saved', row()?.stripe_customer_id?.startsWith('cus_'), row());
  const cid = row().stripe_customer_id;

  // A second checkout reuses the open session with the same price.
  const again = await call('POST', '/ui/subscription/subscribe', { type: 'dynamic-yearly' });
  check('open session reused', again.json.url === r.json.url, again.json);
  const other = await call('POST', '/ui/subscription/subscribe', { type: 'dynamic-monthly' });
  check('other price expires the session', other.json.url && other.json.url !== r.json.url, other.json);

  // The payment arrives: status finds the subscription.
  const stripeSub = await fake(`/__subscribe?customer=${cid}&team_id=${team}&price=price_m`);
  const status = await call('POST', '/ui/subscription/status', {});
  check('status active', status.json.redirect === true, status.json);
  check('status rows', row().stripe_invoice_paid === true && row().stripe_subscription_id === stripeSub.id && row().stripe_plan_id === 'price_m', row());
  const blocked = await call('POST', '/ui/subscription/subscribe', { type: 'dynamic-monthly' });
  check('active subscription refuses checkout', blocked.json.error === 'Team already has an active subscription.', blocked.json);

  // Show.
  await page.goto(`${RS}/subscription/new`);
  await page.waitForTimeout(1000);
  check('index with subscription goes to show', new URL(page.url()).pathname === '/subscription', page.url());
  await page.waitForTimeout(800);
  await page.screenshot({ path: `${shots}sub-show-rs.png` });
  const elig = await call('GET', '/ui/subscription/refund-eligibility');
  check('refund eligibility', elig.json.eligible === true && elig.json.days_remaining === 25 && elig.json.next_billing_date === 'Jan 1, 2030', elig.json);

  // Quantity.
  const prev = await call('POST', '/ui/subscription/preview', { quantity: 5 });
  check('preview', prev.json.preview?.due_now === 700 - 150 && prev.json.preview.recurring_subtotal === 1500 && prev.json.preview.recurring_tax === 150 && prev.json.preview.unit_price === 300 && prev.json.preview.tax_description === 'VAT (EU) 10%' && prev.json.preview.currency === 'USD', prev.json);
  const low = await call('POST', '/ui/subscription/quantity', { quantity: 1 });
  check('minimum', low.json.error === 'Minimum server limit is 2.', low.json);
  const same = await call('POST', '/ui/subscription/quantity', { quantity: 2 });
  check('unchanged', same.json.unchanged === true, same.json);
  await fake('/__unpaid?value=1');
  const unpaid = await call('POST', '/ui/subscription/quantity', { quantity: 7 });
  check('unpaid invoice reverts', unpaid.json.error?.startsWith('Payment failed.') && sql(`SELECT custom_server_limit FROM teams WHERE id = ${team}`) !== '7', unpaid.json);
  log = await fake('/__log');
  check('void and revert calls', log.some((l) => l.path === '/v1/invoices/in_latest/void') && log.some((l) => l.params.proration_behavior === 'none' && l.params['items[0][quantity]'] === '2'), log.slice(-3));
  await fake('/__unpaid?value=0');
  const q = await call('POST', '/ui/subscription/quantity', { quantity: 7 });
  check('quantity updated', q.json.message === 'Server limit updated to 7.' && sql(`SELECT custom_server_limit FROM teams WHERE id = ${team}`) === '7', q.json);

  // Cancel at period end, then resume. A wrong password is refused when one is required.
  const bad = await call('POST', '/ui/subscription/cancel-at-period-end', { password: 'wrong' });
  check('wrong password', bad.json.invalid_password === 'Invalid password.', bad.json);
  const end = await call('POST', '/ui/subscription/cancel-at-period-end', { password: secret });
  check('cancel at period end', end.json.message?.includes('end of the billing period') && row().stripe_cancel_at_period_end === true, end.json);
  const res = await call('POST', '/ui/subscription/resume', {});
  check('resume', res.json.message === 'Subscription resumed successfully.' && row().stripe_cancel_at_period_end === false, res.json);

  // Portal.
  const portal = await call('POST', '/ui/subscription/portal', {});
  check('portal', portal.json.url === 'http://127.0.0.1:1/portal', portal.json);

  // Refund: paid invoice, refund, cancel, rows, one refund per team.
  const refunded = await call('POST', '/ui/subscription/refund', { password: secret });
  check('refund', refunded.json.message === 'Subscription refunded successfully.', refunded.json);
  check('refund rows', row().stripe_refunded_at && row().stripe_feedback === 'Refund requested by user' && row().stripe_subscription_id === null && row().stripe_invoice_paid === false, row());
  log = await fake('/__log');
  check('refund calls', log.some((l) => l.path === '/v1/refunds' && l.params.payment_intent === 'pi_1') && log.some((l) => l.method === 'DELETE' && l.path === `/v1/subscriptions/${stripeSub.id}`), log.slice(-4));
  const cancelled = await call('POST', '/ui/subscription/status', {});
  check('no subscriptions left', cancelled.json.is_cancelled === true, cancelled.json);

  // Cancel immediately on a new subscription.
  const second = await fake(`/__subscribe?customer=${cid}&team_id=${team}&price=price_y`);
  await call('POST', '/ui/subscription/status', {});
  check('yearly interval', (await call('GET', '/ui/subscription')).json.billing_interval === 'yearly', null);
  const now = await call('POST', '/ui/subscription/cancel-now', { password: secret, selected: [] });
  check('cancel now', now.json.message === 'Subscription cancelled successfully.' && row().stripe_feedback === 'Cancelled immediately by user' && row().stripe_subscription_id === null, [now.json, second.id]);

  // Audit events, on this team only.
  const events = sql(`SELECT count(*) FROM audit_events WHERE event LIKE 'ui.subscription.%' AND team_id = ${team}`);
  check('audit events', Number(events) === 5, events);

  // The webhook: signature, then what StripeProcessJob changes.
  const event = async (type, object, sign = true) => {
    const payload = JSON.stringify({ type, data: { object } });
    const t = Math.floor(Date.now() / 1000);
    const hex = createHmac('sha256', sign ? 'whsec_fake' : 'other').update(`${t}.${payload}`).digest('hex');
    const r = await fetch(`${RS}/webhooks/payments/stripe/events`, { method: 'POST', headers: { 'Stripe-Signature': `t=${t},v1=${hex}`, 'Content-Type': 'application/json' }, body: payload });
    await new Promise((d) => setTimeout(d, 300));
    return { status: r.status, text: await r.text() };
  };
  const badSig = await event('invoice.paid', {}, false);
  check('webhook bad signature', badSig.status === 400 && badSig.text === 'Invalid signature.', badSig);
  const user = sql("SELECT id FROM users WHERE email = 'alice@example.test'");
  const okHook = await event('checkout.session.completed', { client_reference_id: `${user}:${team}`, subscription: 'sub_hook', customer: cid });
  check('webhook answer', okHook.status === 200 && okHook.text === 'Webhook received. Cool cool cool cool cool.', okHook);
  check('checkout completed', row().stripe_subscription_id === 'sub_hook' && row().stripe_invoice_paid === true, row());
  const hookItems = { data: [{ subscription: 'sub_hook', quantity: 150, plan: { id: 'price_m' }, price: { lookup_key: 'dynamic-monthly' } }] };
  await event('customer.subscription.updated', { customer: cid, status: 'past_due', cancel_at_period_end: true, cancellation_details: { feedback: 'too_expensive', comment: 'hi' }, items: hookItems });
  check('updated past due', row().stripe_past_due === true && row().stripe_feedback === 'too_expensive' && row().stripe_plan_id === 'price_m' && row().stripe_cancel_at_period_end === true && sql(`SELECT custom_server_limit FROM teams WHERE id = ${team}`) === '100', row());
  await event('customer.subscription.updated', { customer: cid, status: 'active', cancel_at_period_end: false, items: hookItems });
  check('updated active', row().stripe_past_due === false && row().stripe_invoice_paid === true, row());
  await event('customer.subscription.deleted', { customer: cid, id: 'sub_other' });
  check('deleted other subscription ignored', row().stripe_subscription_id === 'sub_hook', row());
  await event('customer.subscription.deleted', { customer: cid, id: 'sub_hook' });
  check('deleted ends the subscription', row().stripe_subscription_id === null && row().stripe_invoice_paid === false, row());
} finally {
  await call('POST', '/ui/session/team', { team_id: before }).catch(() => {});
  if (team) {
    sql(`DELETE FROM subscriptions WHERE team_id = ${team}`);
    sql(`DELETE FROM audit_events WHERE team_id = ${team}`);
    sql(`DELETE FROM team_user WHERE team_id = ${team}`);
    sql(`DELETE FROM teams WHERE id = ${team}`);
  }
  await browser.close();
}
console.log(`pass ${pass}, fail ${fails.length}`);
for (const f of fails) console.log(`FAIL ${f}`);
process.exit(fails.length ? 1 : 0);
