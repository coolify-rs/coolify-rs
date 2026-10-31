// Login, logout, two-factor challenge, forced reset and team two-factor requirement on both sides.
//   node flow_auth.mjs
// Passwords that pass the rules are never sent to the reference: it would ask the Have I Been
// Pwned service about them. It sees weak ones, and the rest is checked with tinker.
import { PASSWORD, audit, auditMax, base, cleanup, createUser, diff, launch, login, open, psql, shot, sides, sameShape, tinker, totp } from './flow_auth_lib.mjs';

const browser = await launch();
cleanup();
const results = {};
const light = process.argv.includes('--light');
try {
  for (const side of sides) {
    const r = (results[side.name] = {});
    const Name = side.name === 'ref' ? 'C Batch Ref' : 'C Batch Rs';
    const mail = (k) => `c-batch-${k}-${side.name}@example.test`;

    // Failed and successful login, logout.
    createUser(mail('plain'), Name);
    let start = auditMax();
    let { ctx, page } = await open(browser, { light });
    await login(page, side, mail('plain'), 'wrong-password');
    await shot(page, 'c-login-failed', side);
    await login(page, side, mail('plain'), 'wrong-password');
    await login(page, side, mail('plain'));
    r.afterLogin = new URL(page.url()).pathname;
    const logout = await page.evaluate(async (isRef) => {
      if (isRef) {
        const token = document.querySelector('meta[name=csrf-token]')?.content;
        const res = await fetch('/logout', { method: 'POST', headers: { 'X-CSRF-TOKEN': token, Accept: 'application/json' } });
        return res.status;
      }
      const s = await (await fetch('/ui/session')).json();
      const res = await fetch('/ui/session', { method: 'DELETE', headers: { 'X-CSRF-Token': s.csrf_token, Accept: 'application/json' } });
      return res.status;
    }, side.name === 'ref');
    r.logoutStatus = logout > 0;
    r.loginAudit = audit(start, `actor_email = '${mail('plain')}' or metadata::text like '%${mail('plain')}%'`);
    await ctx.close();

    // Two-factor challenge: wrong code, recovery form, wrong and right recovery code, then a code.
    const secret = 'JBSWY3DPEHPK3PXP';
    const codes = JSON.stringify(['aaaaaaaaaa-bbbbbbbbbb', 'cccccccccc-dddddddddd']);
    const id2fa = createUser(mail('tfa'), Name, `$u->forceFill(["two_factor_secret" => encrypt("${secret}"), "two_factor_recovery_codes" => encrypt('${codes}'), "two_factor_confirmed_at" => now()])->save();`);
    start = auditMax();
    ({ ctx, page } = await open(browser, { light }));
    await page.goto(base(side) + '/two-factor-challenge');
    await page.waitForTimeout(800);
    r.challengeWithoutLogin = new URL(page.url()).pathname;
    await login(page, side, mail('tfa'));
    r.challengePath = new URL(page.url()).pathname;
    await shot(page, 'c-2fa', side);
    await page.locator('input[name=code]').pressSequentially('000000');
    await page.waitForTimeout(1500);
    await shot(page, 'c-2fa-wrong', side);
    await page.click('button:has-text("Use a recovery code")');
    await shot(page, 'c-2fa-recovery', side);
    await page.fill('input[name=recovery_code]', 'nope');
    await page.click('button[type=submit]');
    await page.waitForTimeout(1500);
    await shot(page, 'c-2fa-recovery-wrong', side);
    await page.click('button:has-text("Use a recovery code")');
    await page.fill('input[name=recovery_code]', 'aaaaaaaaaa-bbbbbbbbbb');
    await page.click('button[type=submit]');
    await page.waitForTimeout(2000);
    r.afterRecovery = new URL(page.url()).pathname;
    const left = tinker(`$c = json_decode(decrypt(App\\Models\\User::find(${id2fa})->two_factor_recovery_codes), true); echo count($c), in_array("aaaaaaaaaa-bbbbbbbbbb", $c) ? " kept" : " replaced", in_array("cccccccccc-dddddddddd", $c) ? " other-kept" : " other-lost";`);
    r.recoveryCodes = left.split('\n').pop();
    await ctx.close();
    ({ ctx, page } = await open(browser, { light }));
    await login(page, side, mail('tfa'));
    await page.locator('input[name=code]').pressSequentially(totp(secret));
    await page.waitForTimeout(2000);
    r.afterCode = new URL(page.url()).pathname;
    r.tfaAudit = audit(start, `actor_email = '${mail('tfa')}'`);
    await ctx.close();

    // Forced password change: the gate, and a password the rules refuse.
    createUser(mail('force'), Name, '$u->forceFill(["force_password_reset" => true])->save();');
    ({ ctx, page } = await open(browser, { light }));
    await login(page, side, mail('force'));
    r.forcePath = new URL(page.url()).pathname;
    await page.goto(base(side) + '/profile');
    await page.waitForTimeout(1200);
    r.forceGate = new URL(page.url()).pathname;
    await shot(page, 'c-force', side);
    await page.fill('input[name=password]', 'weak');
    await page.fill('input[name=password_confirmation]', 'different');
    await page.click('button[type=submit]');
    await page.waitForTimeout(1200);
    await shot(page, 'c-force-weak', side);
    r.forceErrors = (await page.locator('.label-text-alt').allInnerTexts()).map((t) => t.trim());
    await ctx.close();

    // Team requires two-factor: the gate, the steps up to the recovery codes.
    createUser(mail('need'), Name, '$t->is_two_factor_required = true; $t->save();');
    start = auditMax();
    ({ ctx, page } = await open(browser, { light }));
    await login(page, side, mail('need'));
    r.needPath = new URL(page.url()).pathname;
    await page.goto(base(side) + '/profile');
    await page.waitForTimeout(1200);
    r.needGate = new URL(page.url()).pathname;
    await shot(page, 'c-need', side);
    await page.click('button:has-text("Configure 2FA")');
    await page.waitForTimeout(1500);
    r.needConfirmPath = new URL(page.url()).pathname;
    await page.fill('input[name=password]', PASSWORD);
    await page.click('button[type=submit]');
    await page.waitForTimeout(2000);
    r.needAfterConfirm = new URL(page.url()).pathname;
    await page.click('button:has-text("Configure 2FA")').catch(() => {});
    await page.waitForTimeout(1500);
    await shot(page, 'c-need-qr', side);
    await page.fill('input[name=code]', '000000');
    await page.click('button:has-text("Validate 2FA")');
    await page.waitForTimeout(1500);
    await shot(page, 'c-need-wrong', side);
    await page.click('button:has-text("Show manual setup")');
    await shot(page, 'c-need-manual', side);
    const stored = tinker(`echo decrypt(App\\Models\\User::where("email", "${mail('need')}")->first()->two_factor_secret);`).split('\n').pop();
    await page.fill('input[name=code]', totp(stored));
    await page.click('button:has-text("Validate 2FA")');
    await page.waitForTimeout(1500);
    await shot(page, 'c-need-codes', side);
    r.needCodes = await page.locator('.font-mono > div').count();
    await page.click('a:has-text("Continue")');
    await page.waitForTimeout(1500);
    r.needDone = new URL(page.url()).pathname;
    r.needRow = psql(`select two_factor_confirmed_at is not null, two_factor_secret is not null, two_factor_recovery_codes is not null from users where email='${mail('need')}'`);
    r.needAudit = audit(start, `actor_email = '${mail('need')}' and event not like 'auth.user.login%'`);
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
