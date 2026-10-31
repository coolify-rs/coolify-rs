// /profile on both UIs with test users: name, avatar, email change through a local mailpit,
// password rules, two-factor enrollment, passkeys with Chrome's virtual authenticator, and account
// deletion. Comparing toasts, rows and audit_events. Runs on `localhost`, where passkeys work.
// The reference's password rule asks an external service about a valid new password, so a
// successful password change runs on coolify-rs only and the reference gets invalid ones.
//   node compat/ui/flow_profile.mjs [port]
import { createHmac } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { flow, psql, auditMax, auditSince, saveRow, restoreRow } from './flow.mjs';
import { createUser, deleteUser, tinker } from './flow_test_users.mjs';

const port = process.argv[2] ?? '18086';
const NAME = 'batb7';
const PEER = 'batb8';
const png = new URL('./shots/batb-avatar.png', import.meta.url).pathname;
execFileSync('python3', ['-I', '-c', `from PIL import Image; Image.new('RGB',(400,300),(30,90,200)).save('${png}')`]);

let smtp;
const restoreSmtp = () => {
  if (smtp) restoreRow(smtp);
};
const user = (cols) => psql(`select row_to_json(u) from (select ${cols} from users where email like '${NAME}%@example.test' or pending_email is not null and name='${NAME}') u`);

function totp(secret) {
  const alphabet = 'ABCDEFGHIJKLMNOPQRSTUVWXYZ234567';
  let bits = '';
  for (const c of secret) bits += alphabet.indexOf(c).toString(2).padStart(5, '0');
  const bytes = Buffer.from(bits.match(/.{8}/g).map((b) => parseInt(b, 2)));
  const counter = Buffer.alloc(8);
  counter.writeBigUInt64BE(BigInt(Math.floor(Date.now() / 30000)));
  const h = createHmac('sha1', bytes).update(counter).digest();
  const o = h[19] & 15;
  return String((h.readUInt32BE(o) & 0x7fffffff) % 1000000).padStart(6, '0');
}

const scrub = (r) =>
  JSON.stringify(r)
    .replace(/"team_id":\d+/g, '"team_id":"T"')
    .replace(/batb7new\d+/g, 'batb7new')
    .replace(/"(actor_id|member_id)":\d+/g, '"$1":"I"');

function cleanup() {
  for (const n of [NAME, PEER]) deleteUser(n);
  const left = psql(`select string_agg(id::text, ',') from users where name='${NAME}' or email like 'batb7new%'`);
  if (left) {
    for (const id of left.split(',')) {
      const solo = psql(`select string_agg(team_id::text, ',') from team_user where user_id=${id} and team_id<>0`);
      tinker(`DB::table("team_user")->where("user_id",${id})->delete(); ${solo ? `App\\Models\\Team::whereIn("id",[${solo}])->get()->each->delete();` : ''} DB::table("users")->where("id",${id})->delete();`);
      psql(`delete from audit_events where actor_id=${id}`);
      if (solo) psql(`delete from audit_events where team_id in (${solo})`);
    }
  }
}

await flow(
  port,
  NAME,
  async (h) => {
    const rec = {};
    const page = h.page;
    const id = Number(psql(`select id from users where email='${NAME}@example.test'`));
    const start = auditMax();
    const cdp = await page.context().newCDPSession(page);
    await cdp.send('WebAuthn.enable');
    await cdp.send('WebAuthn.addVirtualAuthenticator', { options: { protocol: 'ctap2', transport: 'internal', hasResidentKey: true, hasUserVerification: true, isUserVerified: true, automaticPresenceSimulation: true } });
    const row = (cols) => psql(`select row_to_json(u) from (select ${cols} from users where id=${id}) u`);

    // Details.
    await h.go('/profile');
    await h.shot('pf-initial');
    await h.field('Name').fill('');
    await h.field('Name').press('Enter');
    rec.name_empty = await h.toasts();
    await h.shot('pf-name-dirty', false);
    await h.field('Name').fill(`${NAME}x`);
    await h.field('Name').press('Enter');
    rec.name_saved = await h.toasts();
    rec.name_row = row('name');
    await h.field('Name').fill(NAME);
    await h.field('Name').press('Enter');
    await h.toasts();

    // Avatar.
    await page.locator('input[type=file]').setInputFiles(png);
    await page.waitForTimeout(2000);
    rec.avatar_toasts = await h.toasts();
    await h.shot('pf-avatar');
    rec.avatar_row = row(`avatar_path = 'avatars/' || id || '/avatar.jpg' as path_ok, avatar_storage_type, avatar_s3_storage_id`);
    rec.avatar_fetch = await page.evaluate(async () => {
      const r = await fetch('/profile/avatar?v=1');
      return [r.status, r.headers.get('content-type'), r.headers.get('cache-control')];
    });
    await h.go('/profile');
    await h.button('Remove').click();
    rec.avatar_removed = await h.toasts();
    rec.avatar_row_after = row('avatar_path, avatar_storage_type');

    // Email: no mail configured.
    await h.go('/profile');
    await h.button('Change').click();
    await page.waitForTimeout(300);
    await h.shot('pf-email-modal', false);
    const newEmail = `batb7new${Date.now()}@example.test`;
    const emailInput = page.locator('input[type=email]:visible');
    await emailInput.fill(newEmail);
    await h.button('Send code').click();
    rec.email_not_configured = await h.toasts();

    // With mailpit.
    const host = h.label === 'ref' ? 'host.docker.internal' : '127.0.0.1';
    tinker(`$s = App\\Models\\InstanceSettings::find(0); $s->smtp_enabled = true; $s->smtp_host = "${host}"; $s->smtp_port = 18187; $s->smtp_from_address = "flow@example.test"; $s->smtp_from_name = "Flow"; $s->smtp_encryption = null; $s->smtp_username = null; $s->smtp_password = null; $s->save();`);
    await fetch('http://127.0.0.1:18188/api/v1/messages', { method: 'DELETE' });
    await emailInput.fill(`${PEER}@example.test`);
    await h.button('Send code').click();
    rec.email_taken = await h.toasts();
    await emailInput.fill(newEmail);
    await h.button('Send code').click();
    rec.email_sent = (await h.toasts()).map((t) => t.replace(/batb7new\d+/, 'batb7new'));
    await page.waitForTimeout(500);
    await h.shot('pf-email-verify', false);
    rec.email_row = row(`pending_email is not null as pending, length(email_change_code) as code_len, email_change_code_expires_at > now() at time zone 'utc' as expires_later, email`);
    await h.button('Resend code').click();
    rec.email_resend_early = await h.toasts();
    await h.field('Verification code').fill('12');
    await h.button('Verify email').click();
    rec.email_code_short = await h.toasts();
    await h.field('Verification code').fill('000000');
    await h.button('Verify email').click();
    rec.email_code_wrong = await h.toasts();
    await page.waitForTimeout(1500);
    const list = await (await fetch('http://127.0.0.1:18188/api/v1/messages')).json();
    const m = list.messages?.[0];
    rec.email_mail = m && { subject: m.Subject, to: m.To.map((t) => t.Address.replace(/batb7new\d+/, 'batb7new')) };
    const code = psql(`select email_change_code from users where id=${id}`);
    const body = m && (await (await fetch(`http://127.0.0.1:18188/api/v1/message/${m.ID}`)).json());
    rec.email_mail_has_code = body?.HTML.includes(code);
    await h.field('Verification code').fill(code);
    await h.button('Verify email').click();
    rec.email_verified = await h.toasts();
    await page.waitForTimeout(300);
    rec.email_final = row(`email like 'batb7new%' as moved, pending_email, email_change_code, email_change_code_expires_at`);
    // Back, by SQL: the login name of this flow stays the same for the second side's cleanup.
    psql(`update users set email='${NAME}@example.test' where id=${id}`);

    // A pending change reloads into the verification modal, and closing it cancels.
    psql(`update users set pending_email='batb7pending@example.test', email_change_code='111111', email_change_code_expires_at=(now() at time zone 'utc') + interval '10 minutes' where id=${id}`);
    await h.go('/profile');
    await page.waitForTimeout(300);
    await h.shot('pf-email-pending', false);
    await page.locator('[aria-label="Close"]:visible').first().click();
    rec.email_cancelled = await h.toasts();
    rec.email_cleared = row('pending_email, email_change_code');
    restoreSmtp();

    // Password rules. A valid new password goes to an external service on the reference.
    const pw = async (cur, a, b) => {
      await h.field('Current password').fill(cur);
      await h.field('New password').fill(a);
      await h.field('Confirm new password').fill(b);
      await h.button('Change password').click();
      return h.toasts();
    };
    await h.go('/profile');
    rec.pw_empty = await pw('', '', '');
    rec.pw_short = await pw('x', 'abc', 'abc');
    rec.pw_mismatch = await pw('x', 'abc', 'abd');

    // Two-factor.
    await h.button('Configure 2FA').click();
    await page.waitForTimeout(1500);
    rec.tf_confirm_url = new URL(page.url()).pathname;
    await h.shot('pf-confirm-password', false);
    await page.locator('input[type=password]:visible').fill('wrong-password');
    await page.locator('button[type=submit]:visible').click();
    await page.waitForTimeout(1200);
    await h.shot('pf-confirm-wrong', false);
    rec.tf_confirm_wrong = (await page.locator('.auth-card-body').innerText()).replace(/\s+/g, ' ');
    await page.locator('input[type=password]:visible').fill(h.secret);
    await page.locator('button[type=submit]:visible').click();
    await page.waitForTimeout(1500);
    rec.tf_back_url = new URL(page.url()).pathname;
    await h.button('Configure 2FA').click();
    await page.waitForTimeout(1500);
    await h.shot('pf-2fa-setup');
    rec.tf_setup_row = row('two_factor_secret is not null as secret, two_factor_recovery_codes is not null as codes, two_factor_confirmed_at');
    rec.tf_laravel_reads = tinker(`$u = App\\Models\\User::find(${id}); echo strlen(decrypt($u->two_factor_secret))."/".count($u->recoveryCodes())."/".preg_match("/^[A-Za-z0-9]{10}-[A-Za-z0-9]{10}$/", $u->recoveryCodes()[0]);`);
    await h.button('Show manual setup').click();
    await page.waitForTimeout(200);
    const manual = await page.locator('input[readonly]:visible').evaluateAll((els) => els.map((e) => e.value));
    const secret = manual.find((v) => /^[A-Z2-7]{16}$/.test(v));
    const url = manual.find((v) => v.startsWith('otpauth://'));
    rec.tf_url = url.replace(secret, 'S').replace(NAME, 'N');
    await h.shot('pf-2fa-manual');
    rec.tf_qr = await page.locator('svg[viewBox="0 0 192 192"], svg[width="192"]').count();
    await page.locator('input[name=code]').fill('000000');
    await h.button('Validate 2FA').click();
    await page.waitForTimeout(1500);
    await h.shot('pf-2fa-wrong');
    rec.tf_wrong_visible = await page.locator('text=Profile update failed').count();
    rec.tf_wrong_row = row('two_factor_confirmed_at');
    await h.button('Configure 2FA').click();
    await page.waitForTimeout(1500);
    await page.locator('input[name=code]').fill(totp(secret));
    await h.button('Validate 2FA').click();
    await page.waitForTimeout(1800);
    await h.shot('pf-2fa-codes');
    rec.tf_codes_shown = await page.locator('.font-mono >> div').count();
    rec.tf_confirmed = row('two_factor_confirmed_at is not null as confirmed');
    const before = psql(`select two_factor_recovery_codes from users where id=${id}`);
    await h.button('Regenerate recovery codes').click();
    await page.waitForTimeout(1500);
    rec.tf_regenerated = before !== psql(`select two_factor_recovery_codes from users where id=${id}`);
    rec.tf_codes_again = await page.locator('.font-mono >> div').count();
    await h.go('/profile');
    await h.shot('pf-2fa-enabled');
    rec.tf_codes_after_reload = await page.locator('.font-mono >> div').count();
    await h.button('Disable 2FA').click();
    await page.waitForTimeout(1500);
    await h.shot('pf-2fa-off');
    rec.tf_disabled = row('two_factor_secret, two_factor_recovery_codes, two_factor_confirmed_at');
    rec.tf_toasts = await h.toasts(100);

    // Passkeys.
    await h.go('/profile');
    await h.shot('pf-passkeys-empty');
    await page.locator('a:has-text("Add passkey")').click();
    await page.waitForTimeout(1500);
    rec.pk_url = new URL(page.url()).pathname;
    await h.shot('pf-passkeys-form');
    await h.button('Cancel').click();
    await page.waitForTimeout(400);
    rec.pk_cancel = await page.locator('a:has-text("Add passkey")').count();
    await page.locator('a:has-text("Add passkey")').click();
    await page.waitForTimeout(1500);
    await h.field('Passkey name').fill('Flow key');
    await h.button('Create passkey').click();
    await page.waitForTimeout(2500);
    rec.pk_created = await h.toasts(100);
    await h.shot('pf-passkeys-one');
    const pk = JSON.parse(psql(`select row_to_json(p) from passkeys p where user_id=${id}`));
    rec.pk_row = { ...pk, id: 0, user_id: 0, credential_id: 'C', created_at: 0, updated_at: 0, credential: { ...pk.credential, publicKeyCredentialId: 'C', credentialPublicKey: 'K', userHandle: 'H' } };
    rec.pk_keys_order = Object.keys(pk.credential);
    rec.pk_id_match = pk.credential_id === pk.credential.publicKeyCredentialId;
    rec.pk_handle = Buffer.from(pk.credential.userHandle, 'base64url').toString() === tinker(`echo App\\Models\\User::find(${id})->getPasskeyUserHandle();`);
    await h.button('Rename').click();
    await page.waitForTimeout(200);
    await h.shot('pf-passkeys-rename');
    await page.locator('input:visible[aria-label="Passkey name"]').fill('Renamed key');
    await h.button('Save').click();
    await page.waitForTimeout(1200);
    rec.pk_renamed = await h.toasts(100);
    rec.pk_name = psql(`select name from passkeys where user_id=${id}`);
    await h.go('/profile');
    await page.locator('section', { hasText: 'Passkeys' }).getByRole('button', { name: 'Delete', exact: true }).click();
    await page.waitForTimeout(400);
    await h.shot('pf-passkeys-delete', false);
    await page.locator('input:visible').last().fill('Renamed key');
    // The recent password confirmation skips the password step.
    await page.getByRole('button', { name: 'Confirm', exact: true }).click();
    await page.waitForTimeout(1200);
    rec.pk_deleted = await h.toasts(100);
    rec.pk_left = psql(`select count(*) from passkeys where user_id=${id}`);

    // Account deletion.
    await h.go('/profile');
    await h.shot('pf-delete-state');
    await h.button('Delete account').click();
    await page.waitForTimeout(400);
    await h.shot('pf-delete-modal', false);
    await page.locator('input:visible').last().fill(`${NAME}@example.test`);
    await page.getByRole('button', { name: 'Confirm', exact: true }).click();
    await page.waitForTimeout(2500);
    rec.del_url = new URL(page.url()).pathname;
    rec.del_user = psql(`select count(*) from users where id=${id}`);
    rec.del_pivot = psql(`select count(*) from team_user where user_id=${id}`);
    rec.del_team = psql(`select count(*) from teams where name='${NAME}''s Team' or name='${NAME}x''s Team'`);

    rec.audit = auditSince(start)
      .filter((r) => r.actor_id === id && !r.event.startsWith('auth.'))
      .map(scrub);
    psql(`delete from audit_events where actor_id=${id}`);

    // A successful password change, on coolify-rs only (see the top of this file).
    if (h.label === 'rs') {
      const c = await page.context().browser().newContext({ viewport: { width: 1440, height: 900 }, colorScheme: 'dark' });
      const p = await c.newPage();
      const peer = Number(psql(`select id from users where email='${PEER}@example.test'`));
      await p.goto(h.base + '/login');
      await p.fill('input[name=email]', `${PEER}@example.test`);
      await p.fill('input[name=password]', h.secret);
      await p.click('button[type=submit]');
      await p.waitForTimeout(1500);
      await p.goto(h.base + '/profile');
      await p.waitForTimeout(1000);
      const field = (l) => p.locator('label:visible', { hasText: l }).first().locator('xpath=ancestor::div[.//input][1]//input').first();
      await field('Current password').fill('wrong-password');
      await field('New password').fill('Newpass1!x');
      await field('Confirm new password').fill('Newpass1!x');
      await p.getByRole('button', { name: 'Change password', exact: true }).click();
      await p.waitForTimeout(1000);
      console.log('rs wrong current password toast:', await p.locator('.toast-card').first().innerText().then((t) => t.replace(/\s+/g, ' ')));
      const s = auditMax();
      await field('Current password').fill(h.secret);
      await p.getByRole('button', { name: 'Change password', exact: true }).click();
      await p.waitForTimeout(2000);
      console.log('rs after password change at', new URL(p.url()).pathname);
      console.log('rs laravel checks the new hash:', tinker(`echo Illuminate\\Support\\Facades\\Hash::check("Newpass1!x", App\\Models\\User::find(${peer})->password) ? "yes" : "no";`));
      console.log('rs audit:', JSON.stringify(auditSince(s).filter((r) => r.actor_id === peer).map((r) => [r.event, r.description, r.metadata])));
      await p.fill('input[name=email]', `${PEER}@example.test`);
      await p.fill('input[name=password]', 'Newpass1!x');
      await p.click('button[type=submit]');
      await p.waitForTimeout(1500);
      console.log('rs login with the new password at', new URL(p.url()).pathname);
      await c.close();
    }
    return rec;
  },
  {
    host: 'localhost',
    before: () => {
      cleanup();
      createUser(NAME);
      createUser(PEER);
      smtp = saveRow('instance_settings', 'id=0');
    },
    after: () => {
      restoreSmtp();
      cleanup();
    },
  },
);
