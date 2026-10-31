// Notification pages: same steps on both UIs, comparing toasts, rows and audit_events.
//   node compat/ui/flow_notifications.mjs [port] [channel ...]
import { flow, psql, saveRow, restoreRow, auditMax, auditSince, tokens } from './flow.mjs';

const port = process.argv[2] ?? '18086';
const only = process.argv.slice(3);
const TEAM = 4;

/** The row through the rs API, which decrypts, plus the raw columns other than ciphertext. */
async function row(channel) {
  const r = await fetch(`http://127.0.0.1:${port}/api/v1/notifications/${channel}`, { headers: { Authorization: `Bearer ${tokens.acme_owner_root}` } });
  const j = await r.json();
  const failure = psql(`select failure_count from ${channel}_notification_settings where team_id = ${TEAM}`);
  return { ...j, failure_count: failure };
}

const CHANNELS = {
  discord: {
    url: '/notifications/discord',
    fields: [['Webhook URL', 'http://10.99.99.99:9/discord']],
    bad: [['Webhook URL', 'not a url']],
    blocked: [['Webhook URL', 'http://127.0.0.1/x']],
    events: ['discord-deployments-events', 'discord-servers-events'],
    extra: async (h, rec) => {
      await h.listbox('Critical event mention', 'Do not mention');
      rec.ping_toast = await h.toasts();
      rec.ping_row = await row('discord');
    },
  },
  slack: { url: '/notifications/slack', fields: [['Webhook URL', 'http://10.99.99.99:9/slack']], bad: [['Webhook URL', 'nope']], blocked: [['Webhook URL', 'http://localhost/x']], events: ['slack-backups-events'] },
  webhook: { url: '/notifications/webhook', fields: [['Webhook URL', 'http://10.99.99.99:9/hook']], bad: [['Webhook URL', 'ftp://x']], blocked: [['Webhook URL', 'http://169.254.169.254/x']], events: ['webhook-resources-events'] },
  pushover: {
    url: '/notifications/pushover',
    fields: [
      ['User key', 'uk'],
      ['API token', 'at'],
    ],
    events: ['pushover-scheduled-tasks-events'],
  },
  telegram: {
    url: '/notifications/telegram',
    fields: [
      ['Bot API token', 'tok'],
      ['Chat ID', '-100'],
    ],
    events: ['telegram-deployments-events'],
    extra: async (h, rec) => {
      await h.page.getByLabel('Deployment failure topic ID').fill('77');
      await h.page.getByLabel('Deployment failure topic ID').press('Enter');
      rec.thread_toast = await h.toasts();
      rec.thread_row = await row('telegram');
    },
  },
};

async function channelFlow(h, channel, c) {
  const rec = {};
  const start = auditMax();
  await h.go(c.url);
  // Enabling with no credentials.
  await h.button('Enable').click();
  rec.enable_empty = await h.toasts();
  rec.enable_empty_row = await row(channel);
  for (const [l, v] of c.bad ?? []) {
    await h.field(l).fill(v);
    await h.enterIn(l);
    rec.bad_url = await h.toasts();
  }
  for (const [l, v] of c.blocked ?? []) {
    await h.field(l).fill(v);
    await h.enterIn(l);
    rec.blocked_url = await h.toasts();
    rec.blocked_row = await row(channel);
  }
  for (const [l, v] of c.fields) {
    await h.field(l).fill(v);
  }
  await h.enterIn(c.fields[0][0]);
  rec.save = await h.toasts();
  rec.save_row = await row(channel);
  await h.button('Enable').click();
  rec.enable = await h.toasts();
  rec.enable_row = await row(channel);
  await h.page.waitForTimeout(300);
  if (channel !== 'telegram' && channel !== 'pushover') {
    // Telegram and Pushover tests call the real services, so a flow never sends them.
    await h.button('Send test').click();
    rec.test = await h.toasts();
  }
  for (const id of c.events) {
    const trigger = h.page.locator(`#${id}-trigger`);
    await trigger.click();
    await h.page.waitForTimeout(150);
    await h.page.locator('.listbox-option:visible').first().click();
    await h.page.waitForTimeout(400);
    await h.page.keyboard.press('Escape');
    rec[`event_${id}`] = await h.toasts();
    rec[`event_row_${id}`] = await row(channel);
    await h.shot(`nf-${channel}-after`);
  }
  await c.extra?.(h, rec);
  await h.button('Disable').click();
  rec.disable = await h.toasts();
  rec.disable_row = await row(channel);
  rec.audit = auditSince(start, '^ui\\.notifications');
  return rec;
}

// A private address nothing listens on passes SafeWebhookUrl only through the allowlist.
const instance = psql("select json_build_object('a', webhook_allow_localhost, 'h', webhook_allowed_internal_hosts) from instance_settings where id = 0");
psql(`update instance_settings set webhook_allowed_internal_hosts = '["10.99.99.99"]' where id = 0`);
process.on('exit', () => {
  const i = JSON.parse(instance);
  psql(`update instance_settings set webhook_allow_localhost = ${i.a}, webhook_allowed_internal_hosts = ${i.h === null ? 'NULL' : `'${JSON.stringify(i.h)}'`} where id = 0`);
});

for (const [channel, c] of Object.entries(CHANNELS)) {
  if (only.length && !only.includes(channel)) continue;
  const saved = saveRow(`${channel}_notification_settings`, `team_id = ${TEAM}`);
  console.log(`== ${channel}`);
  await flow(port, 'alice', (h) => channelFlow(h, channel, c), { before: () => restoreRow(saved), after: () => restoreRow(saved) });
}

if (!only.length || only.includes('email')) {
  const saved = saveRow('email_notification_settings', `team_id = ${TEAM}`);
  console.log('== email');
  await flow(
    port,
    'alice',
    async (h) => {
      const rec = {};
      const start = auditMax();
      await h.go('/notifications/email');
      await h.shot('nf-email-empty');
      await h.listbox('SMTP delivery', 'Enabled');
      rec.smtp_empty = await h.toasts();
      await h.page.waitForTimeout(300);
      rec.smtp_empty_row = await row('email');
      await h.go('/notifications/email');
      await h.field('From name').fill('Diff');
      await h.field('From address').fill('nope');
      await h.field('From name').press('Enter');
      rec.bad_email = await h.toasts();
      await h.field('From address').fill('diff@example.test');
      await h.field('Host').fill('127.0.0.1');
      await h.page.locator('input[placeholder="587"]').fill('70000');
      await h.field('From name').press('Enter');
      rec.bad_port = await h.toasts();
      await h.page.locator('input[placeholder="587"]').fill('1');
      await h.page.locator('input[placeholder="coolify.example.com"]').fill('bad host');
      await h.field('From name').press('Enter');
      rec.bad_ehlo = await h.toasts();
      await h.page.locator('input[placeholder="coolify.example.com"]').fill('');
      await h.listbox('SMTP delivery', 'Enabled');
      rec.smtp_no_encryption = await h.toasts();
      rec.smtp_no_encryption_row = await row('email');
      await h.go('/notifications/email');
      await h.field('From name').fill('Diff');
      await h.field('From address').fill('diff@example.test');
      await h.field('Host').fill('127.0.0.1');
      await h.page.locator('input[placeholder="587"]').fill('1');
      await h.listbox('Encryption', 'None');
      await h.field('SMTP username').fill('u');
      await h.field('SMTP password').fill('p');
      await h.field('From name').press('Enter');
      rec.save = await h.toasts();
      rec.save_row = await row('email');
      await h.listbox('SMTP delivery', 'Enabled');
      rec.smtp_on = await h.toasts();
      rec.smtp_on_row = await row('email');
      await h.page.waitForTimeout(400);
      await h.shot('nf-email-smtp');
      await h.button('Send test').click();
      await h.page.waitForTimeout(400);
      await h.shot('nf-email-modal', false);
      await h.page.locator('input[placeholder="test@example.com"]').fill('dest@example.test');
      await h.button('Send email').click();
      rec.test_refused = await h.toasts(2500);
      await h.button('Send test').click();
      await h.page.waitForTimeout(300);
      await h.button('Send email').click();
      rec.test_rate = await h.toasts(1500);
      await h.listbox('Resend delivery', 'Enabled');
      rec.resend_nokey = await h.toasts();
      rec.resend_nokey_row = await row('email');
      await h.go('/notifications/email');
      await h.field('API key').fill('re_x');
      await h.listbox('Resend delivery', 'Enabled');
      rec.resend_on = await h.toasts();
      rec.resend_on_row = await row('email');
      await h.event('deployment-email-events', 'Deployment success');
      rec.event = await h.toasts();
      rec.event_row = await row('email');
      await h.page.locator('.form-control', { hasText: 'Include commit details' }).locator('input').click();
      rec.commit = await h.toasts();
      rec.commit_row = await row('email');
      await h.listbox('Email service', 'Use system-wide settings');
      rec.instance = await h.toasts();
      rec.instance_row = await row('email');
      await h.page.waitForTimeout(400);
      await h.shot('nf-email-instance');
      rec.audit = auditSince(start, '^ui\\.notifications');
      return rec;
    },
    { before: () => restoreRow(saved), after: () => restoreRow(saved) },
  );
}

if (!only.length || only.includes('member')) {
  // Bob is a plain member of Acme: secrets hidden, controls disabled.
  console.log('== member');
  await flow(port, 'bob', async (h) => {
    await h.go('/notifications/discord');
    await h.switchTeam("Bob Member's Team", 'Acme');
    const rec = {};
    for (const channel of ['email', 'discord', 'telegram']) {
      await h.go(`/notifications/${channel}`);
      await h.shot(`nf-member-${channel}`);
      rec[`${channel}_disabled`] = await h.page.locator('input:disabled, button:disabled').count();
    }
    return rec;
  });
}

if (!only.length || only.includes('root')) {
  // An instance admin sees "Copy from instance settings": Alice joins the root team for this run.
  const saved = saveRow('email_notification_settings', `team_id = ${TEAM}`);
  const instanceRow = saveRow('instance_settings', 'id = 0');
  console.log('== instance admin');
  psql("insert into team_user (team_id, user_id, role) values (0, 1, 'admin')");
  psql("update instance_settings set smtp_from_name = NULL, smtp_host = NULL, smtp_port = 2525, smtp_encryption = 'tls', smtp_timeout = 7, smtp_ehlo_domain = 'mail.example.test', smtp_enabled = true where id = 0");
  try {
    await flow(
      port,
      'alice',
      async (h) => {
        const rec = {};
        const start = auditMax();
        await h.go('/notifications/email');
        await h.shot('nf-root-email');
        await h.page.locator('button:visible', { hasText: 'Copy from instance settings' }).first().click();
        rec.copy = await h.toasts();
        rec.row = await row('email');
        rec.audit = auditSince(start, '^ui\\.notifications');
        return rec;
      },
      { before: () => restoreRow(saved), after: () => restoreRow(saved) },
    );
  } finally {
    psql('delete from team_user where team_id = 0 and user_id = 1');
    restoreRow(instanceRow);
  }
}
