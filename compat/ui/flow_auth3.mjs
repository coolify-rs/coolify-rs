// Passkey sign-in and confirmation with Chrome's virtual authenticator, on localhost, both sides.
//   node flow_auth3.mjs
import { PASSWORD, audit, auditMax, base, cleanup, createUser, diff, launch, login, open, psql, shot, sides, sameShape } from './flow_auth_lib.mjs';

const light = process.argv.includes('--light');
const browser = await launch();
cleanup();
const results = {};
const HOST = 'localhost';

async function authenticator(ctx, page) {
  const cdp = await ctx.newCDPSession(page);
  await cdp.send('WebAuthn.enable');
  await cdp.send('WebAuthn.addVirtualAuthenticator', {
    options: { protocol: 'ctap2', transport: 'internal', hasResidentKey: true, hasUserVerification: true, isUserVerified: true, automaticPresenceSimulation: true },
  });
}

try {
  for (const side of sides) {
    const r = (results[side.name] = {});
    const Name = side.name === 'ref' ? 'C Batch Ref' : 'C Batch Rs';
    const email = `c-batch-pass-${side.name}@example.test`;
    createUser(email, Name);
    const { ctx, page } = await open(browser, { light });
    await authenticator(ctx, page);

    // The login page offers a passkey on a host with a relying party.
    await page.goto(base(side, HOST) + '/login');
    await page.waitForTimeout(1500);
    await shot(page, 'c-login-passkey', side);
    r.autocomplete = await page.getAttribute('input[name=email]', 'autocomplete');
    r.button = await page.locator('button:has-text("Sign in with a passkey")').count();

    // No passkey yet: the browser finds none for this site.
    await login(page, side, email, PASSWORD, HOST);
    r.afterLogin = new URL(page.url()).pathname;

    // Add one on /profile.
    await page.goto(base(side, HOST) + '/profile');
    await page.waitForTimeout(1500);
    await page.click('a:has-text("Add passkey")');
    await page.waitForTimeout(1500);
    r.confirmPath = new URL(page.url()).pathname;
    await page.fill('input[name=password]', PASSWORD);
    await page.click('button[type=submit]');
    await page.waitForTimeout(2000);
    await page.locator('button:has-text("Add passkey"), button:has-text("Create passkey"), button:has-text("Register")').last().click();
    await page.waitForTimeout(2500);
    r.row = JSON.parse(psql(`select row_to_json(x) from (select p.name is not null as named, p.last_used_at is null as unused, (p.credential->>'counter') as counter, json_object_keys_list from passkeys p, lateral (select string_agg(k, ',') as json_object_keys_list from json_object_keys(p.credential) k) y where p.user_id = (select id from users where email = '${email}')) x`) || 'null');

    // Sign out, sign in with the passkey.
    const context2 = ctx;
    await context2.clearCookies();
    let start = auditMax();
    await page.goto(base(side, HOST) + '/login');
    // The virtual authenticator answers the email field's autofill request by itself.
    await page.click('button:has-text("Sign in with a passkey")', { timeout: 1500 }).catch(() => {});
    await page.waitForTimeout(3000);
    r.afterPasskey = new URL(page.url()).pathname;
    r.used = psql(`select last_used_at is not null, credential->>'counter' from passkeys where user_id = (select id from users where email = '${email}')`).replace(/\|\d+$/, '|N');
    r.loginAudit = audit(start, `actor_email = '${email}'`);

    // The confirmation page has the passkey button.
    await page.goto(base(side, HOST) + '/user/confirm-password');
    await page.waitForSelector('button:has-text("Confirm with a passkey")');
    await shot(page, 'c-confirm-passkey', side);
    start = auditMax();
    await page.click('button:has-text("Confirm with a passkey")');
    await page.waitForTimeout(3000);
    r.afterConfirm = new URL(page.url()).pathname;
    r.confirmAudit = audit(start, `actor_email = '${email}'`);

    // A guest whose challenge was used: an old assertion cannot be replayed.
    const replay = await page.evaluate(async (isRs) => {
      const url = isRs ? '/ui/auth/passkeys/login' : '/passkeys/login';
      const res = await fetch(url, { method: 'POST', headers: { 'Content-Type': 'application/json', Accept: 'application/json', 'X-CSRF-TOKEN': document.querySelector('meta[name=csrf-token]')?.content ?? '' }, body: JSON.stringify({ credential: { id: 'x', rawId: 'eA', type: 'public-key', response: {} } }) });
      return res.status;
    }, side.name === 'rs');
    r.replayStatus = replay;
    await ctx.close();
  }
  const { ref, rs } = results;
  for (const k of Object.keys(ref)) {
    if (Array.isArray(ref[k])) diff(k, ref[k], rs[k], sameShape);
    else console.log(`${JSON.stringify(ref[k]) === JSON.stringify(rs[k]) ? 'SAME ' : 'DIFF '} ${k}: ${JSON.stringify(ref[k])} | ${JSON.stringify(rs[k])}`);
  }
} finally {
  await browser.close();
  cleanup();
  console.log('left:', psql(`select count(*) from users where email like 'c-batch-%@example.test'`));
}
