// The rest of the sign-in checks: a forced reset to the end on coolify-rs, with the result signing
// in on the reference, the login throttle's page on both sides, and sign-out from the gates.
//   node flow_auth5.mjs
import { PASSWORD, audit, auditMax, base, cleanup, createUser, launch, login, open, psql, shot, sides, sameShape } from './flow_auth_lib.mjs';

const light = process.argv.includes('--light');
const NEW_PASSWORD = 'C-batch-Other2!';
const browser = await launch();
cleanup();
try {
  const [ref, rs] = sides;

  // Forced reset through coolify-rs; a valid password is never sent to the reference.
  const email = 'c-batch-force-rs@example.test';
  createUser(email, 'C Batch Rs', '$u->forceFill(["force_password_reset" => true])->save();');
  const start = auditMax();
  let { ctx, page } = await open(browser, { light });
  await login(page, rs, email);
  await page.fill('input[name=password]', NEW_PASSWORD);
  await page.fill('input[name=password_confirmation]', NEW_PASSWORD);
  await page.click('button[type=submit]');
  await page.waitForTimeout(2000);
  console.log('after forced reset:', new URL(page.url()).pathname);
  console.log('row:', psql(`select force_password_reset, substr(password, 1, 4) from users where email = '${email}'`));
  console.log('audit:', JSON.stringify(audit(start, `actor_email = '${email}' and event like 'ui.%'`).map(sameShape)));
  await ctx.close();
  ({ ctx, page } = await open(browser, { light }));
  await login(page, ref, email, NEW_PASSWORD);
  console.log('new password on the reference:', new URL(page.url()).pathname);
  await ctx.close();

  // The sixth attempt of a minute.
  for (const side of sides) {
    const address = `c-batch-throttle-${side.name}@example.test`;
    ({ ctx, page } = await open(browser, { light }));
    for (let i = 0; i < 6; i++) await login(page, side, address, 'wrong-password');
    await shot(page, 'c-login-throttled', side);
    console.log(side.name, 'throttled page:', await page.locator('.error-code').innerText().catch(() => 'none'));
    console.log(side.name, 'failed rows of the minute:', psql(`select count(*) from audit_events where event = 'auth.user.login_failed' and metadata->>'attempted_email' = '${address}'`));
    await ctx.close();
  }

  // Log out from the page a user is stuck on.
  for (const side of sides) {
    const address = `c-batch-need-${side.name}@example.test`;
    createUser(address, 'C Batch X', '$t->is_two_factor_required = true; $t->save();');
    ({ ctx, page } = await open(browser, { light }));
    await login(page, side, address);
    await page.click('button:has-text("Log out")');
    await page.waitForTimeout(1500);
    console.log(side.name, 'after log out:', new URL(page.url()).pathname);
    await ctx.close();
  }
} finally {
  await browser.close();
  cleanup();
  console.log('left:', psql(`select count(*) from users where email like 'c-batch-%@example.test'`));
}
