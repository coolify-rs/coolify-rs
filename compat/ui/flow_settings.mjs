// Instance settings pages on both sides: toasts, rows and audit events.
// The user must be an instance admin (a test user in team 0). The whole instance_settings row is
// saved before each side and put back right after. No fqdn, API or allowed IP success path runs here:
// servers 21 and 57 follow the instance URL, and other engineers use the API.
//   node compat/ui/flow_settings.mjs 18086 batd1
import { flow, psql, saveRow, restoreRow, auditMax, auditSince } from './flow.mjs';

const [port, user] = process.argv.slice(2);
let saved;
const row = (cols) => JSON.parse(psql(`select row_to_json(x) from (select ${cols} from instance_settings where id=0) x`));
const clean = (rows) => rows.map((r) => ({ ...r, description: undefined }));

await flow(port, user, async (h) => {
  const r = {};
  const start = auditMax();
  // General: validation errors.
  await h.go('/settings');
  await h.field('URL').fill('not a url');
  await h.page.getByRole('button', { name: /Save changes/ }).first().click();
  r.badUrl = await h.toasts();
  await h.field('URL').fill('');
  await h.field('Instance public IPv4').fill('999.1.1.1');
  await h.field('Instance public IPv6').fill('zzz');
  await h.page.getByRole('button', { name: /Save changes/ }).first().click();
  r.badIps = await h.toasts();
  // General: name saved, then a timezone through the listbox.
  await h.go('/settings');
  await h.field('Name').fill('batd instance');
  await h.page.getByRole('button', { name: /Save changes/ }).first().click();
  await h.page.waitForTimeout(1500);
  r.nameRow = row('instance_name, fqdn');
  await h.go('/settings');
  await h.page.locator('#instance_timezone-trigger').first().click();
  await h.page.waitForTimeout(200);
  await h.page.keyboard.type('Europe/Berlin');
  await h.page.waitForTimeout(200);
  await h.page.locator('.listbox-option:visible', { hasText: 'Europe/Berlin' }).first().click().catch(() => {});
  await h.page.waitForTimeout(2500);
  r.tzRow = row('instance_timezone');
  r.generalAudit = clean(auditSince(start, 'instance.settings'));
  await h.shot('s-gen-after');

  // Advanced: validation errors first.
  const a0 = auditMax();
  await h.go('/settings/advanced');
  await h.field('Allowed IPs').fill('nope, 10.0.0.0/99');
  await h.page.getByRole('button', { name: /Save changes/ }).first().click();
  r.badAllowed = await h.toasts();
  await h.field('Allowed IPs').fill('');
  await h.field('Custom DNS servers').fill('nope');
  await h.page.getByRole('button', { name: /Save changes/ }).first().click();
  r.badDns = await h.toasts();
  await h.field('Custom DNS servers').fill('1.1.1.1, 1.1.1.1,');
  await h.page.locator('textarea[placeholder*="hooks.company"]').fill('-bad-');
  await h.page.getByRole('button', { name: /Save changes/ }).first().click();
  r.badHooks = await h.toasts();
  await h.page.locator('textarea[placeholder*="hooks.company"]').fill('Hooks.Company.Local., 10.50.0.0/16\nhooks.company.local');
  await h.field('Image CDN URL').fill('https://images.example.test/');
  await h.page.getByRole('button', { name: /Save changes/ }).first().click();
  r.okAdvanced = await h.toasts();
  r.advRow = row('custom_dns_servers, allowed_ips, webhook_allowed_internal_hosts, image_cdn_url');
  await h.listbox('Sponsorship reminders', 'Disabled');
  r.instantToast = await h.toasts();
  r.instantRow = row('is_sponsorship_popup_enabled');
  await h.listbox('Passkeys', 'Disabled');
  r.passkey = await h.toasts();
  r.passkeyRow = row('is_passkey_enabled');
  r.advAudit = clean(auditSince(a0, 'instance.settings'));

  // Updates: validation only. A success writes REGISTRY_URL on localhost, which fails here.
  const u0 = auditMax();
  await h.go('/settings/updates');
  await h.field('Check frequency').fill('not a cron');
  await h.page.getByRole('button', { name: /Save changes/ }).first().click();
  r.badCron = await h.toasts();
  await h.field('Check frequency').fill('');
  await h.page.getByRole('button', { name: /Save changes/ }).first().click();
  await h.page.waitForTimeout(800);
  r.emptyCron = await h.page.locator('text=The update check frequency field is required.').count();
  await h.field('Check frequency').fill('15 * * * *');
  await h.page.getByRole('button', { name: /Save changes/ }).first().click();
  r.cronSave = await h.toasts(5000);
  r.updRow = row('update_check_frequency, docker_registry_url');
  r.updAudit = clean(auditSince(u0, 'instance.settings'));

  // Email: validation, a save, then SMTP.
  const e0 = auditMax();
  await h.go('/settings/email');
  await h.field('From address').fill('nope');
  await h.page.getByRole('button', { name: /Save changes/ }).first().click();
  r.badFrom = await h.toasts();
  await h.field('From address').fill('from@example.test');
  await h.field('From name').fill('Batch D');
  await h.field('Port').fill('70000');
  await h.page.getByRole('button', { name: /Save changes/ }).first().click();
  r.badPort = await h.toasts();
  await h.field('Port').fill('1025');
  await h.field('Host').fill('127.0.0.1');
  await h.field('EHLO domain').fill('bad domain');
  await h.page.getByRole('button', { name: /Save changes/ }).first().click();
  r.badEhlo = await h.toasts();
  await h.field('EHLO domain').fill('');
  await h.page.getByRole('button', { name: /Save changes/ }).first().click();
  r.okEmail = await h.toasts();
  r.emailRow = row('smtp_enabled, smtp_port, smtp_encryption, smtp_timeout, resend_enabled, smtp_from_address is not null as has_from');
  await h.listbox('SMTP delivery', 'Enabled');
  r.smtpMissing = await h.toasts();
  r.emailRow2 = row('smtp_enabled, resend_enabled');
  await h.listbox('Encryption', 'None');
  await h.listbox('SMTP delivery', 'Enabled');
  r.smtpOn = await h.toasts();
  r.emailRow3 = row('smtp_enabled, resend_enabled, smtp_encryption');
  await h.listbox('Resend delivery', 'Enabled');
  r.resendMissing = await h.toasts();
  r.emailRow4 = row('smtp_enabled, resend_enabled');
  r.emailAudit = clean(auditSince(e0, 'settings.email'));

  // Backup: not ready on both sides here.
  await h.go('/settings/backup');
  r.backupText = (await h.page.locator('.application-settings-section').first().innerText()).replace(/\s+/g, ' ');
  return r;
}, {
  before: () => (saved = saveRow('instance_settings', 'id=0')),
  after: () => restoreRow(saved),
});
