// Help and the test email through a local mailpit, the domain conflict modal, the upgrade modal, and
// on coolify-rs only the paths that would leave this machine on the reference: Detect, Check now and
// the feedback fake. The instance_settings row is saved and restored around each side.
// Mailpit: docker run -d --name batd-mailpit -p 127.0.0.1:18195:8025 -p 127.0.0.1:18194:1025 axllent/mailpit
// The fake listener answers on 18197 and the instance has its COOLIFY_RS_* URLs pointed at it.
//   node compat/ui/flow_mail.mjs 18086 batd1
import { flow, psql, saveRow, restoreRow, auditMax, auditSince } from './flow.mjs';
import { tinker } from './flow_test_users.mjs';

const [port, user] = process.argv.slice(2);
let saved;
const mails = async () => {
  const r = await fetch('http://127.0.0.1:18195/api/v1/messages').then((x) => x.json());
  return r.messages;
};
const mailBody = async (id) => {
  const m = await fetch(`http://127.0.0.1:18195/api/v1/message/${id}`).then((x) => x.json());
  return { subject: m.Subject, to: m.To.map((t) => t.Address), from: m.From.Address, html: m.HTML.trim() };
};
const clearMails = () => fetch('http://127.0.0.1:18195/api/v1/messages', { method: 'DELETE' });

await flow(
  port,
  user,
  async (h) => {
    const r = {};
    const start = auditMax();
    // Help: validation, on both sides, with no mail settings. A valid form would go to Coolify's API.
    await h.go('/');
    await h.page.locator('.app-sidebar button:has-text("batd1")').last().click();
    await h.page.waitForTimeout(300);
    await h.page.locator('.listbox-option:visible', { hasText: 'Feedback' }).click();
    await h.page.waitForTimeout(400);
    await h.shot('m-help', false);
    await h.page.locator('input[placeholder*="short summary"]:visible').fill('ab');
    await h.page.locator('textarea[placeholder*="trying to do"]:visible').fill('short');
    await h.page.locator('input[placeholder*="short summary"]:visible').evaluate((el) => (el.closest('form').noValidate = true));
    await h.page.locator('button:visible', { hasText: 'Send feedback' }).click();
    r.helpInvalid = await h.toasts(1200);
    // With SMTP: the mail goes to mailpit.
    await clearMails();
    const host = h.label === 'ref' ? 'host.docker.internal' : '127.0.0.1';
    tinker(`$s = App\\Models\\InstanceSettings::find(0); $s->smtp_enabled = true; $s->smtp_host = "${host}"; $s->smtp_port = 18194; $s->smtp_from_address = "flow@example.test"; $s->smtp_from_name = "Flow"; $s->smtp_encryption = "none"; $s->save();`);
    await h.page.locator('input[placeholder*="short summary"]:visible').fill('Batch D subject');
    await h.page.locator('textarea[placeholder*="trying to do"]:visible').fill('Details <b>of</b> the problem & more.');
    await h.page.locator('button:visible', { hasText: 'Send feedback' }).click();
    r.helpSent = await h.toasts(2500);
    let list = await mails();
    r.helpMail = list[0] ? await mailBody(list[0].ID) : null;
    if (r.helpMail) r.helpMail.to = r.helpMail.to.map((t) => t.replace(/^feedback@.*/, 'feedback'));
    await h.page.keyboard.press('Escape');

    // The test email of /settings/email.
    await clearMails();
    await h.go('/settings/email');
    await h.page.waitForTimeout(500);
    await h.shot('m-email-on', true);
    await h.page.locator('button:visible', { hasText: 'Send test' }).first().click();
    await h.page.waitForTimeout(300);
    await h.shot('m-email-test', false);
    await h.page.locator('button:visible', { hasText: 'Send email' }).first().click();
    r.testToast = await h.toasts(2500);
    list = await mails();
    r.testMail = list[0] ? { subject: (await mailBody(list[0].ID)).subject, to: (await mailBody(list[0].ID)).to } : null;
    await h.page.locator('button:visible', { hasText: 'Send test' }).first().click();
    await h.page.waitForTimeout(200);
    await h.page.locator('button:visible', { hasText: 'Send email' }).first().click();
    r.testAgain = await h.toasts(1500);
    r.emailAudit = auditSince(start, 'settings.email').map((a) => a.metadata);

    // Conflict modal: DNS validation off for the moment, so nothing asks 1.1.1.1. Nothing is saved.
    psql(`update instance_settings set is_dns_validation_enabled=false where id=0`);
    await h.go('/settings');
    await h.field('URL').fill('https://dk-diff-1.example.com');
    await h.page.waitForTimeout(200);
    await h.shot('m-gen-https', true);
    await h.page.locator('button:visible', { hasText: 'Save changes' }).first().click();
    await h.page.waitForTimeout(1500);
    r.conflict = (await h.page.locator('h3:visible', { hasText: 'Domain already in use' }).count()) > 0;
    r.conflictText = (await h.page.locator('.application-settings-section-body:visible li').allInnerTexts()).map((s) => s.replace(/\s+/g, ' '));
    await h.shot('m-conflict', false);
    await h.page.locator('button:visible', { hasText: 'Cancel' }).first().click();
    r.fqdnAfter = psql(`select coalesce(fqdn,'null') from instance_settings where id=0`);

    // The upgrade modal: opened and closed. "Upgrade now" inside is never pressed.
    await h.go('/settings/updates');
    await h.page.locator('button:visible', { hasText: /Upgrade now/i }).first().click();
    await h.page.waitForTimeout(300);
    await h.shot('m-upgrade', false);
    await h.page.keyboard.press('Escape');
    await h.page.getByRole('button', { name: 'Close', exact: true }).first().click({ timeout: 800 }).catch(() => {});

    if (h.label === 'rs') {
      // Local fakes only.
      await h.go('/settings');
      await h.page.getByRole('button', { name: 'Detect', exact: true }).click();
      console.log('rs detect', await h.toasts(2000), psql(`select public_ipv4 from instance_settings where id=0`));
      await h.go('/settings/updates');
      await h.page.getByRole('button', { name: /Check now/i }).click();
      console.log('rs check', await h.toasts(2000), psql(`select new_version_available from instance_settings where id=0`));
      psql(`update instance_settings set smtp_enabled=false where id=0`);
      await h.go('/');
      await h.page.locator('.app-sidebar button:has-text("batd1")').last().click();
      await h.page.waitForTimeout(300);
      await h.page.locator('.listbox-option:visible', { hasText: 'Feedback' }).click();
      await h.page.waitForTimeout(400);
      await h.page.locator('input[placeholder*="short summary"]:visible').fill('Fake subject');
      await h.page.locator('textarea[placeholder*="trying to do"]:visible').fill('Sent to the local fake.');
      await h.page.locator('button:visible', { hasText: 'Send feedback' }).click();
      console.log('rs feedback', await h.toasts(2000));
    }
    return r;
  },
  {
    before: () => (saved = saveRow('instance_settings', 'id=0')),
    after: () => restoreRow(saved),
  },
);
