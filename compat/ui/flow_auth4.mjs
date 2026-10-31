// SettingsOauth and the OAuth round trip against a local fake Authentik, on both sides. Fixed-URL
// providers only have their redirect inspected: nothing goes to an external site.
//   node flow_auth4.mjs
// The oauth_settings rows and the registration columns are saved first and restored exactly.
import http from 'node:http';
import { PASSWORD, audit, auditMax, base, cleanup, createUser, diff, launch, login, open, psql, shot, sides, sameShape, tinker, totp } from './flow_auth_lib.mjs';

const light = process.argv.includes('--light');
const FAKE = 18187;

// ---- the fake provider ----
const fake = { claims: {}, requests: [] };
const server = http.createServer(async (req, res) => {
  const url = new URL(req.url, `http://localhost:${FAKE}`);
  let body = '';
  for await (const c of req) body += c;
  if (url.pathname === '/application/o/authorize/') {
    fake.requests.push({ at: 'authorize', params: Object.fromEntries(url.searchParams) });
    const back = new URL(url.searchParams.get('redirect_uri'));
    back.searchParams.set('code', 'fake-code');
    back.searchParams.set('state', url.searchParams.get('state'));
    res.writeHead(302, { Location: back.toString() }).end();
  } else if (url.pathname === '/application/o/token/') {
    fake.requests.push({ at: 'token', params: Object.fromEntries(new URLSearchParams(body)) });
    res.writeHead(200, { 'Content-Type': 'application/json' }).end(JSON.stringify({ access_token: 'fake-token', token_type: 'bearer', expires_in: 300 }));
  } else if (url.pathname === '/application/o/userinfo/') {
    fake.requests.push({ at: 'userinfo', auth: req.headers.authorization });
    res.writeHead(200, { 'Content-Type': 'application/json' }).end(JSON.stringify(fake.claims));
  } else res.writeHead(404).end();
});
await new Promise((r) => server.listen(FAKE, '0.0.0.0', r));

// ---- saved state ----
const savedOauth = JSON.parse(psql(`select json_agg(t) from oauth_settings t`));
const settingsCols = ['is_registration_enabled', 'disable_registration_when_oauth_enabled', 'updated_at'];
const savedSettings = JSON.parse(psql(`select row_to_json(t) from (select ${settingsCols.join(',')} from instance_settings) t`));
const lit = (v) => (v === null ? 'NULL' : `'${String(v).replace(/'/g, "''")}'`);
function restore() {
  for (const row of savedOauth) {
    psql(`update oauth_settings set ${Object.keys(row).filter((k) => k !== 'id').map((k) => `"${k}" = ${lit(row[k])}`).join(', ')} where id = ${row.id}`);
  }
  psql(`update instance_settings set ${settingsCols.map((c) => `${c} = ${lit(savedSettings[c])}`).join(', ')}`);
}
const setProvider = (provider, fields) =>
  tinker(`$o = App\\Models\\OauthSetting::where("provider", "${provider}")->first(); $o->forceFill(${JSON.stringify(fields).replace(/":/g, '" =>').replace(/^\{/, '[').replace(/\}$/, ']')})->save();`);

const browser = await launch();
cleanup();
const results = {};
const provRows = (email) =>
  JSON.parse(psql(`select coalesce(json_agg(x), '[]') from (select i.provider, regexp_replace(i.issuer, ':[0-9]+$', '') as issuer_base, i.provider_user_id, i.email, i.raw_claims::text as raw_claims, i.last_login_at is not null as logged_in from oauth_identities i join users u on u.id = i.user_id where u.email = '${email}' order by i.id) x`)).map((r) => ({ ...r, issuer_base: r.issuer_base.replace('host.docker.internal', 'H').replace('127.0.0.1', 'H') }));

try {
  for (const side of sides) {
    const r = (results[side.name] = {});
    const Name = side.name === 'ref' ? 'C Batch Ref' : 'C Batch Rs';
    const mail = (k) => `c-batch-${k}-${side.name}@example.test`;
    const hostForServer = side.name === 'ref' ? 'host.docker.internal' : '127.0.0.1';
    const fakeBase = `http://${hostForServer}:${FAKE}`;

    // ---- the settings page, as an instance admin of my own ----
    createUser(mail('admin'), Name, '$u->teams()->attach(0, ["role" => "owner"]); $u->forceFill(["current_team_id" => $t->id])->save();');
    let start = auditMax();
    let { ctx, page } = await open(browser, { light });
    await login(page, side, mail('admin'));
    await page.goto(base(side) + '/settings/oauth');
    await page.waitForSelector('#authentik-oauth-section');
    await page.waitForTimeout(800);
    await shot(page, 'c-oauth', side);
    await page.click('#authentik-oauth-section button:has-text("Enable")');
    await page.waitForTimeout(800);
    r.validityBlocks = psql(`select enabled from oauth_settings where provider = 'authentik'`);
    await page.fill('input[name="oauth_settings_map.authentik.redirect_uri"]', 'not a url');
    await page.fill('input[name="oauth_settings_map.authentik.client_id"]', 'fake-client');
    await page.fill('input[name="oauth_settings_map.authentik.client_secret"]', 'fake-secret');
    await page.fill('input[name="oauth_settings_map.authentik.base_url"]', fakeBase);
    await page.click('#authentik-oauth-section button:has-text("Enable")');
    await page.waitForTimeout(1500);
    await shot(page, 'c-oauth-invalid', side);
    r.invalidToast = (await page.locator('[role=status], .toast').allInnerTexts()).join(' ').slice(0, 200);
    await page.waitForTimeout(500);
    await page.goto(base(side) + '/settings/oauth/authentik');
    await page.waitForSelector('#authentik-oauth-section');
    await page.fill('input[name="oauth_settings_map.authentik.redirect_uri"]', `${base(side)}/auth/authentik/callback`);
    await page.fill('input[name="oauth_settings_map.authentik.client_id"]', 'fake-client');
    await page.fill('input[name="oauth_settings_map.authentik.client_secret"]', 'fake-secret');
    await page.fill('input[name="oauth_settings_map.authentik.base_url"]', fakeBase);
    await page.click('#authentik-oauth-section button:has-text("Enable")');
    await page.waitForTimeout(2000);
    await shot(page, 'c-oauth-enabled', side);
    r.enabledRow = JSON.parse(psql(`select row_to_json(x) from (select enabled, client_id, redirect_uri is not null as has_redirect, regexp_replace(base_url, 'http://[^:]+', 'http://H') as base, scopes, custom_label, allow_registration, auto_join_root_team, require_email_verified, use_pkce, clock_skew_seconds from oauth_settings where provider = 'authentik') x`));
    r.secretReadable = tinker(`echo App\\Models\\OauthSetting::where("provider", "authentik")->first()->client_secret;`).split('\n').pop();
    r.settingsAudit = audit(start, `event like 'ui.oauth_setting%' and actor_email = '${mail('admin')}'`);
    start = auditMax();
    await page.click('input#disable_registration_when_oauth_enabled, input[name=disable_registration_when_oauth_enabled]', { force: true }).catch(() => {});
    await page.waitForTimeout(1000);
    await ctx.close();
    psql(`update instance_settings set disable_registration_when_oauth_enabled = false`);

    // ---- the login page with a provider ----
    ({ ctx, page } = await open(browser, { light }));
    await page.goto(base(side) + '/login');
    await page.waitForSelector('button:has-text("Login with Authentik")');
    await shot(page, 'c-login-oauth', side);

    // ---- a new user ----
    start = auditMax();
    fake.requests = [];
    fake.claims = { sub: `sub-new-${side.name}`, email: mail('new').toUpperCase(), email_verified: true, name: Name, preferred_username: 'c/batch', groups: ['a'] };
    await page.click('button:has-text("Login with Authentik")');
    await page.waitForTimeout(3000);
    r.newUserPath = new URL(page.url()).pathname;
    await shot(page, 'c-oauth-new', side);
    r.authorize = fake.requests.find((x) => x.at === 'authorize')?.params;
    r.authorize.state = r.authorize.state?.length;
    r.authorize.redirect_uri = r.authorize.redirect_uri.replace(/:\d+/, ':P');
    const token = fake.requests.find((x) => x.at === 'token')?.params;
    token.redirect_uri = token?.redirect_uri?.replace(/:\d+/, ':P');
    r.token = token;
    r.userinfoAuth = fake.requests.find((x) => x.at === 'userinfo')?.auth;
    r.newRows = provRows(mail('new')).map((x) => ({ ...x, provider_user_id: x.provider_user_id.replace(side.name, 'X'), email: x.email.replace(side.name, 'X'), raw_claims: x.raw_claims.replace(/-(ref|rs)/g, '-X').replace(/C Batch (Ref|Rs)/, 'C Batch X') }));
    r.newUser = JSON.parse(psql(`select row_to_json(x) from (select u.name is not null n, u.email_verified_at is not null verified, u.created_before_oauth_identities, t.personal_team, t.show_boarding, tu.role from users u join team_user tu on tu.user_id = u.id join teams t on t.id = tu.team_id where u.email = '${mail('new')}') x`));
    r.newAudit = audit(start, `actor_email = '${mail('new')}'`);
    await ctx.close();

    // ---- the same identity again ----
    start = auditMax();
    ({ ctx, page } = await open(browser, { light }));
    await page.goto(base(side) + '/login');
    await page.click('button:has-text("Login with Authentik")');
    await page.waitForTimeout(3000);
    r.againAudit = audit(start, `actor_email = '${mail('new')}'`);
    r.againIdentities = provRows(mail('new')).length;
    await ctx.close();

    // ---- an existing account: the email must be verified ----
    createUser(mail('link'), Name);
    start = auditMax();
    fake.claims = { sub: `sub-link-${side.name}`, email: mail('link'), email_verified: false, name: Name };
    ({ ctx, page } = await open(browser, { light }));
    await page.goto(base(side) + '/login');
    await page.click('button:has-text("Login with Authentik")');
    await page.waitForTimeout(3000);
    r.unverifiedPath = new URL(page.url()).pathname;
    await shot(page, 'c-oauth-unverified', side);
    r.unverifiedError = await page.locator('.auth-card-body p').first().innerText();
    r.unverifiedAudit = audit(start, `event = 'auth.oauth.callback_failed' and user_agent like '%c-batch-flow%'`).map((x) => ({ ...x, metadata: { ...x.metadata } }));
    // Verified: links.
    start = auditMax();
    fake.claims = { sub: `sub-link-${side.name}`, email: mail('link'), email_verified: true, name: Name };
    await page.click('button:has-text("Login with Authentik")');
    await page.waitForTimeout(3000);
    r.linkPath = new URL(page.url()).pathname;
    r.linkAudit = audit(start, `actor_email = '${mail('link')}'`).map((x) => JSON.parse(JSON.stringify(x).replace(/http:\/\/[^:"]+:\d+/g, 'ISSUER')));
    await ctx.close();
    // A second identity for a linked account is refused.
    fake.claims = { sub: `sub-other-${side.name}`, email: mail('link'), email_verified: true, name: Name };
    ({ ctx, page } = await open(browser, { light }));
    await page.goto(base(side) + '/login');
    await page.click('button:has-text("Login with Authentik")');
    await page.waitForTimeout(3000);
    r.linkedError = await page.locator('.auth-card-body p').first().innerText();
    await ctx.close();

    // ---- two-factor users get the challenge ----
    const secret = 'JBSWY3DPEHPK3PXP';
    tinker(`$u = App\\Models\\User::where("email", "${mail('link')}")->first(); $u->forceFill(["two_factor_secret" => encrypt("${secret}"), "two_factor_recovery_codes" => encrypt("[]"), "two_factor_confirmed_at" => now()])->save();`);
    fake.claims = { sub: `sub-link-${side.name}`, email: mail('link'), email_verified: true, name: Name };
    start = auditMax();
    ({ ctx, page } = await open(browser, { light }));
    await page.goto(base(side) + '/login');
    await page.click('button:has-text("Login with Authentik")');
    await page.waitForTimeout(3000);
    r.tfaPath = new URL(page.url()).pathname;
    await page.locator('input[name=code]').pressSequentially(totp(secret));
    await page.waitForTimeout(2000);
    r.tfaDone = new URL(page.url()).pathname;
    r.tfaAudit = audit(start, `actor_email = '${mail('link')}'`);
    await ctx.close();

    // ---- registration off: no new users, and the footer says so ----
    psql(`update instance_settings set is_registration_enabled = false`);
    fake.claims = { sub: `sub-closed-${side.name}`, email: mail('closed'), email_verified: true, name: Name };
    ({ ctx, page } = await open(browser, { light }));
    await page.goto(base(side) + '/login');
    await page.waitForSelector('button:has-text("Login with Authentik")');
    await shot(page, 'c-login-closed', side);
    r.closedFooter = await page.locator('.auth-card-footer').innerText();
    await page.goto(base(side) + '/register');
    await page.waitForTimeout(1000);
    r.registerRedirect = new URL(page.url()).pathname;
    await page.click('button:has-text("Login with Authentik")');
    await page.waitForTimeout(3000);
    r.closedError = await page.locator('.auth-card-body p').first().innerText();
    r.closedUser = psql(`select count(*) from users where email = '${mail('closed')}'`);
    await ctx.close();
    psql(`update instance_settings set is_registration_enabled = ${lit(savedSettings.is_registration_enabled)}`);

    // ---- auto-join: a member of the root team, without a personal team ----
    setProvider('authentik', { auto_join_root_team: true });
    fake.claims = { sub: `sub-root-${side.name}`, email: mail('root'), email_verified: true, name: Name };
    ({ ctx, page } = await open(browser, { light }));
    await page.goto(base(side) + '/login');
    await page.click('button:has-text("Login with Authentik")');
    await page.waitForTimeout(3000);
    r.rootMembership = psql(`select string_agg(tu.team_id || ':' || tu.role, ',') from team_user tu join users u on u.id = tu.user_id where u.email = '${mail('root')}'`);
    await ctx.close();
    setProvider('authentik', { auto_join_root_team: false });

    // ---- bad state ----
    start = auditMax();
    ({ ctx, page } = await open(browser, { light }));
    await page.goto(`${base(side)}/auth/authentik/callback?code=x&state=forged`);
    await page.waitForTimeout(1500);
    r.badStatePath = new URL(page.url()).pathname;
    r.badStateError = await page.locator('.auth-card-body p').first().innerText();
    r.badStateAudit = audit(start, `event = 'auth.oauth.callback_failed' and user_agent like '%c-batch-flow%'`);
    await shot(page, 'c-oauth-badstate', side);
    // A provider that is off: a 403 page.
    await page.goto(`${base(side)}/auth/discord/redirect`);
    await page.waitForTimeout(1000);
    await shot(page, 'c-oauth-off', side);
    r.offStatus = (await page.evaluate(() => fetch(location.href, { redirect: 'manual' }).then((x) => x.status)));
    await ctx.close();

    // ---- fixed-URL providers: the 302 only ----
    const location = {};
    for (const [provider, fields] of [
      ['github', {}],
      ['gitlab', {}],
      ['google', { tenant: 'example.com' }],
      ['discord', {}],
      ['bitbucket', {}],
      ['infomaniak', {}],
      ['azure', { tenant: 'c-batch-tenant' }],
      ['clerk', { base_url: 'https://c-batch.example.test' }],
      ['zitadel', { base_url: 'https://c-batch.example.test' }],
    ]) {
      setProvider(provider, { client_id: 'c-batch-id', client_secret: 'c-batch-secret', enabled: true, redirect_uri: 'http://c-batch.example.test/cb', ...fields });
      const res = await fetch(`${base(side)}/auth/${provider}/redirect`, { redirect: 'manual' });
      location[provider] = `${res.status} ${(res.headers.get('location') ?? '').replace(/state=[A-Za-z0-9]{40}/, 'state=S')}`;
      setProvider(provider, { client_id: null, client_secret: null, enabled: false, redirect_uri: null, tenant: null, base_url: null });
    }
    r.locations = location;
    setProvider('authentik', { client_id: null, client_secret: null, enabled: false, redirect_uri: null, base_url: null, scopes: null });
    restore();
    cleanup();
  }
  const { ref, rs } = results;
  for (const k of Object.keys(ref)) {
    if (Array.isArray(ref[k]) && ref[k].length && typeof ref[k][0] === 'object') diff(k, ref[k], rs[k], sameShape);
    else {
      const a = JSON.stringify(ref[k]);
      const b = JSON.stringify(rs[k]);
      console.log(a === b ? `SAME  ${k}: ${a.slice(0, 300)}` : `DIFF  ${k}\n  ref ${a}\n  rs  ${b}`);
    }
  }
} finally {
  await browser.close();
  server.close();
  restore();
  cleanup();
  console.log('left:', psql(`select count(*) from users where email like 'c-batch-%@example.test'`), 'identities:', psql(`select count(*) from oauth_identities`));
  console.log('oauth enabled:', psql(`select count(*) from oauth_settings where enabled`), 'registration:', psql(`select is_registration_enabled, disable_registration_when_oauth_enabled from instance_settings`));
}
