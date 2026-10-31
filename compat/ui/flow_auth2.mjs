// Registration, password reset with a local mailpit, and email verification on both sides.
//   node flow_auth2.mjs   (mailpit on 18125/18126, created and removed by the caller)
// The instance's SMTP columns are saved first and restored exactly. Passwords that pass the rules
// are never sent to the reference (Have I Been Pwned): it gets weak ones and tinker does the rest.
import { PASSWORD, audit, auditMax, base, cleanup, createUser, diff, launch, login, open, psql, shot, sides, sameShape, tinker } from './flow_auth_lib.mjs';

const light = process.argv.includes('--light');
const NEW_PASSWORD = 'C-batch-Other2!';
const columns = ['smtp_enabled', 'smtp_host', 'smtp_port', 'smtp_from_address', 'smtp_from_name', 'smtp_encryption', 'smtp_username', 'smtp_password', 'smtp_timeout', 'resend_enabled', 'updated_at'];
const saved = JSON.parse(psql(`select row_to_json(t) from (select ${columns.join(',')} from instance_settings) t`));
const lit = (v) => (v === null ? 'NULL' : `'${String(v).replace(/'/g, "''")}'`);
const restore = () => psql(`update instance_settings set ${columns.map((c) => `${c} = ${lit(saved[c])}`).join(', ')}`);
const mailTo = (host) => tinker(`$s = instanceSettings(); $s->forceFill(['smtp_enabled' => true, 'resend_enabled' => false, 'smtp_host' => '${host}', 'smtp_port' => 18125, 'smtp_from_address' => 'c-batch@example.test', 'smtp_from_name' => 'C Batch', 'smtp_encryption' => null, 'smtp_username' => null, 'smtp_password' => null, 'smtp_timeout' => 10])->save();`);
const mails = async () => (await (await fetch('http://127.0.0.1:18126/api/v1/messages')).json()).messages ?? [];
const clearMails = () => fetch('http://127.0.0.1:18126/api/v1/messages', { method: 'DELETE' });
async function mailFor(to) {
  for (let i = 0; i < 60; i++) {
    const m = (await mails()).find((x) => x.To.some((t) => t.Address === to));
    if (m) return (await fetch(`http://127.0.0.1:18126/api/v1/message/${m.ID}`)).json();
    await new Promise((r) => setTimeout(r, 500));
  }
  return null;
}
const hrefOf = (html, pattern) => html.match(pattern)?.[0].replace(/&amp;/g, '&');

const browser = await launch();
cleanup();
const results = {};
try {
  for (const side of sides) {
    const r = (results[side.name] = {});
    const Name = side.name === 'ref' ? 'C Batch Ref' : 'C Batch Rs';
    const mail = (k) => `c-batch-${k}-${side.name}@example.test`;
    await clearMails();
    mailTo(side.name === 'ref' ? 'host.docker.internal' : '127.0.0.1');

    // Registration: what the rules refuse; a weak password is safe on the reference.
    createUser(mail('taken'), Name);
    let { ctx, page } = await open(browser, { light });
    await page.goto(base(side) + '/register');
    await page.waitForSelector('input[name=name]');
    await page.fill('input[name=name]', Name);
    await page.fill('input[name=email]', mail('taken'));
    await page.fill('input[name=password]', 'weak');
    await page.fill('input[name=password_confirmation]', 'other');
    await page.click('button[type=submit]');
    await page.waitForTimeout(1500);
    await shot(page, 'c-register-errors', side);
    r.registerErrors = (await page.locator('.auth-card-body p').allInnerTexts()).filter((t) => t.startsWith('The '));
    await ctx.close();

    // A new account.
    let start = auditMax();
    if (side.name === 'rs') {
      ({ ctx, page } = await open(browser, { light }));
      await page.goto(base(side) + '/register');
      await page.waitForSelector('input[name=name]');
      await page.fill('input[name=name]', Name);
      await page.fill('input[name=email]', mail('new'));
      await page.fill('input[name=password]', PASSWORD);
      await page.fill('input[name=password_confirmation]', PASSWORD);
      await page.click('button[type=submit]');
      await page.waitForTimeout(2000);
      r.registerPath = new URL(page.url()).pathname;
      await shot(page, 'c-register-done', side);
      await ctx.close();
    } else {
      // CreateNewUser with the verifier faked, in a process that cannot reach the service.
      tinker(
        `app()->instance(Illuminate\\Contracts\\Validation\\UncompromisedVerifier::class, new class implements Illuminate\\Contracts\\Validation\\UncompromisedVerifier { public function verify($data) { return true; } });
         $u = (new App\\Actions\\Fortify\\CreateNewUser)->create(["name" => "${Name}", "email" => "${mail('new')}", "password" => "${PASSWORD}", "password_confirmation" => "${PASSWORD}"]);
         event(new Illuminate\\Auth\\Events\\Registered($u)); echo $u->id;`,
      );
      r.registerPath = '/onboarding';
    }
    r.registerRows = JSON.parse(
      psql(`select json_agg(x) from (select u.name, u.email, u.email_verified_at is not null as verified, u.force_password_reset, u.marketing_emails, t.name as team, t.personal_team, t.show_boarding, t.is_mcp_server_enabled, t.is_build_server_fallback_enabled, t.is_two_factor_required, tu.role,
        (select count(*) from email_notification_settings e where e.team_id = t.id) + (select count(*) from discord_notification_settings e where e.team_id = t.id) as settings
        from users u join team_user tu on tu.user_id = u.id join teams t on t.id = tu.team_id where u.email = '${mail('new')}') x`),
    );
    r.registerAudit = audit(start, `actor_email = '${mail('new')}' and event = 'auth.user.registered'`).map(({ team_id, event, level, actor_name, metadata }) => ({ team_id, event, level, actor_name, metadata }));
    r.bcrypt = psql(`select substr(password, 1, 4) from users where email = '${mail('new')}'`);

    // Forgot password: unknown address, sent, sent again within a minute.
    createUser(mail('reset'), Name);
    ({ ctx, page } = await open(browser, { light }));
    await page.goto(base(side) + '/login');
    await page.waitForTimeout(1000);
    await shot(page, 'c-login-mail', side);
    await page.click('a:has-text("Forgot password?")');
    await page.waitForSelector('input[name=email]');
    await shot(page, 'c-forgot-form', side);
    await page.fill('input[name=email]', 'c-batch-unknown@example.test');
    await page.click('button[type=submit]');
    await page.waitForTimeout(1500);
    await shot(page, 'c-forgot-unknown', side);
    await page.fill('input[name=email]', mail('reset'));
    await page.click('button[type=submit]');
    await page.waitForTimeout(2000);
    await shot(page, 'c-forgot-sent', side);
    r.forgotStatus = await page.locator('.auth-card-body').innerText();
    const row = psql(`select substr(token, 1, 4), created_at is not null from password_reset_tokens where email = '${mail('reset')}'`);
    r.tokenRow = row;
    const message = await mailFor(mail('reset'));
    r.resetMailSubject = message?.Subject;
    const link = hrefOf(message?.HTML ?? '', /[a-z]+:\/\/[^"]*\/reset-password\/[^"]+/);
    r.resetMailText = (message?.Text ?? '').replace(/\S*reset-password\S*/g, 'LINK');
    const token = link?.match(/reset-password\/([^?]+)/)?.[1];
    r.resetTokenLength = token?.length;
    r.tokenAcceptedByUpstream = tinker(`$u = App\\Models\\User::where("email", "${mail('reset')}")->first(); echo Illuminate\\Support\\Facades\\Password::broker()->tokenExists($u, "${token}") ? "yes" : "no";`).split('\n').pop();

    // The reset page: a bad token, and a good token with a weak password.
    await page.goto(`${base(side)}/reset-password/not-a-token?email=${encodeURIComponent(mail('reset'))}`);
    await page.waitForSelector('input[name=password]');
    await page.fill('input[name=password]', 'weak');
    await page.fill('input[name=password_confirmation]', 'weak');
    await page.click('button[type=submit]');
    await page.waitForTimeout(1500);
    await shot(page, 'c-reset-bad-token', side);
    r.resetBadToken = await page.locator('.auth-card-body p').first().innerText();
    r.resetBadTokenPath = new URL(page.url()).pathname.replace(/\/[^/]+$/, '');
    await page.goto(`${base(side)}/reset-password/${token}?email=${encodeURIComponent(mail('reset'))}`);
    await page.waitForSelector('input[name=password]');
    await page.fill('input[name=password]', 'weak');
    await page.fill('input[name=password_confirmation]', 'weak');
    await page.click('button[type=submit]');
    await page.waitForTimeout(1500);
    await shot(page, 'c-reset-weak', side);
    r.resetWeak = (await page.locator('.auth-card-body p').allInnerTexts()).filter((t) => t.startsWith('The '));
    await ctx.close();

    // The token of this side is used on coolify-rs in both runs, and the result on the reference.
    start = auditMax();
    if (side.name === 'ref') r.crossToken = token;
    ({ ctx, page } = await open(browser, { light }));
    const rsSide = sides[1];
    await page.goto(`${base(rsSide)}/reset-password/${token}?email=${encodeURIComponent(mail('reset'))}`);
    await page.waitForSelector('input[name=password]');
    await page.fill('input[name=password]', NEW_PASSWORD);
    await page.fill('input[name=password_confirmation]', NEW_PASSWORD);
    await page.click('button[type=submit]');
    await page.waitForTimeout(2000);
    r.afterResetPath = new URL(page.url()).pathname;
    await shot(page, `c-reset-done-${side.name}`, side);
    r.afterResetStatus = await page.locator('.auth-card-body').first().innerText();
    await ctx.close();
    r.resetAudit = audit(start, `actor_email = '${mail('reset')}'`).map(({ team_id, event, level, actor_name, metadata }) => ({ team_id, event, level, actor_name, metadata }));
    r.tokenGone = psql(`select count(*) from password_reset_tokens where email = '${mail('reset')}'`);
    // The password coolify-rs wrote signs in on the reference.
    ({ ctx, page } = await open(browser, { light }));
    await login(page, sides[0], mail('reset'), NEW_PASSWORD);
    r.referenceLoginAfterReset = new URL(page.url()).pathname;
    await ctx.close();

    // Email verification.
    await clearMails();
    createUser(mail('verify'), Name, '$u->forceFill(["email_verified_at" => null])->save();');
    start = auditMax();
    ({ ctx, page } = await open(browser, { light }));
    await login(page, side, mail('verify'));
    r.unverifiedLandsOn = new URL(page.url()).pathname;
    await page.goto(base(side) + '/verify');
    await page.waitForSelector('button:has-text("Resend")');
    await shot(page, 'c-verify', side);
    await page.click('button:has-text("Resend")');
    await page.waitForTimeout(2000);
    await shot(page, 'c-verify-sent', side);
    await page.click('button:has-text("Resend")');
    await page.waitForTimeout(1000);
    await shot(page, 'c-verify-limited', side);
    const verifyMail = await mailFor(mail('verify'));
    r.verifyMailSubject = verifyMail?.Subject;
    const verifyLink = hrefOf(verifyMail?.HTML ?? '', /[a-z]+:\/\/[^"]*\/email\/verify\/[^"]+/);
    r.verifyMailText = (verifyMail?.Text ?? '').replace(/\S*email\/verify\S*/g, 'LINK');
    r.verifySignatureAcceptedByUpstream = tinker(`echo Illuminate\\Support\\Facades\\URL::hasValidSignature(Illuminate\\Http\\Request::create("${verifyLink}")) ? "yes" : "no";`).split('\n').pop();
    // Signed for this side's address with the shared key, as the mail is for the instance's.
    const uid = psql(`select id from users where email = '${mail('verify')}'`);
    const local = tinker(`$g = clone Illuminate\\Support\\Facades\\URL::getFacadeRoot(); $g->forceRootUrl("${base(side)}"); $g->forceScheme("http"); echo $g->temporarySignedRoute("verify.verify", now()->addMinutes(60), ["id" => ${uid}, "hash" => hash("sha256", "${mail('verify')}")]);`).split('\n').pop();
    await page.goto(local.replace(/signature=.{8}/, 'signature=00000000'));
    await page.waitForTimeout(1200);
    await shot(page, 'c-verify-bad', side);
    r.badSignature = await page.locator('.error-code').innerText().catch(() => 'none');
    await page.goto(local);
    await page.waitForTimeout(1500);
    r.afterVerify = new URL(page.url()).pathname;
    r.verified = psql(`select email_verified_at is not null from users where email = '${mail('verify')}'`);
    r.verifyAudit = audit(start, `actor_email = '${mail('verify')}' and event = 'auth.user.email_verified'`);
    await ctx.close();
  }
  const { ref, rs } = results;
  for (const k of Object.keys(ref)) {
    if (k === 'crossToken') continue;
    if (Array.isArray(ref[k])) diff(k, ref[k], rs[k], sameShape);
    else console.log(`${JSON.stringify(ref[k]) === JSON.stringify(rs[k]) ? 'SAME ' : 'DIFF '} ${k}: ${JSON.stringify(ref[k])} | ${JSON.stringify(rs[k])}`);
  }
} finally {
  await browser.close();
  cleanup();
  restore();
  console.log('left:', psql(`select count(*) from users where email like 'c-batch-%@example.test'`), 'teams:', psql(`select count(*) from teams where name like 'C Batch%'`));
  console.log('smtp restored:', psql(`select smtp_enabled, smtp_host from instance_settings`));
}
