// A real interactive shell in a browser for every resource kind and the server.
//   node terminal_ui.mjs [--port=18086]   (resources from compat/diff/terminal_fixture.py up)
// Each kind: the session opens, a command runs in a TTY, a resize reaches it, `exit` ends it.
// The login password is read from the fixture file and never printed.
import { chromium } from 'playwright-core';
import fs from 'node:fs';

const args = process.argv.slice(2);
const opt = (k, d) => (args.find((a) => a.startsWith(`--${k}=`)) ?? `=${d}`).split('=')[1];
const base = opt('port', 18086).toString().startsWith('http') ? opt('port') : `http://127.0.0.1:${opt('port', 18086)}`;
const fx = JSON.parse(fs.readFileSync(new URL('./shots/terminal-fixture.json', import.meta.url)));
const secret = fs.readFileSync(new URL('../reference/secrets/ui-login.txt', import.meta.url), 'utf8').match(/^password:\s*(.+)$/m)[1].trim();
const shots = new URL('./shots/', import.meta.url).pathname;
const side = opt("label", "rs");

let pass = 0;
let fail = 0;
const check = (name, ok, detail = '') => {
  ok ? pass++ : fail++;
  console.log(`${ok ? 'PASS' : 'FAIL'} ${name}${ok ? '' : ' ' + String(detail).slice(0, 400)}`);
};

const browser = await chromium.launch({ executablePath: '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', headless: true });
const ctx = await browser.newContext({ viewport: { width: 1440, height: 900 }, colorScheme: 'dark' });
const page = await ctx.newPage();
await page.addInitScript(() => { try { localStorage.setItem('theme', 'dark'); } catch {} });
let frames = [];
page.on('websocket', (ws) => {
  if (!ws.url().endsWith('/terminal/ws')) return;
  ws.on('framesent', (f) => frames.push(['>', String(f.payload)]));
  ws.on('framereceived', (f) => frames.push(['<', String(f.payload)]));
});
const errors = [];
page.on('pageerror', (e) => errors.push(e.message));
await page.goto(base + '/login');
await page.fill('input[name=email]', 'alice@example.test');
await page.fill('input[name=password]', secret);
await Promise.all([page.waitForURL((u) => !u.pathname.startsWith('/login'), { timeout: 15000 }).catch(() => {}), page.click('button[type=submit]')]);

const screen = () => page.evaluate(() => document.querySelector('#terminal .xterm-rows')?.innerText ?? '');
const until = async (fn, ms = 20000) => {
  const end = Date.now() + ms;
  while (Date.now() < end) {
    if (await fn()) return true;
    await page.waitForTimeout(150);
  }
  return false;
};
const output = () => frames.filter((f) => f[0] === '<').map((f) => f[1]).join('');

async function shell(label, opts = {}) {
  check(`${label}: pty-ready`, await until(() => frames.some((f) => f[1] === 'pty-ready')), JSON.stringify(frames.slice(0, 5)));
  await page.waitForTimeout(800);
  await page.keyboard.type('echo MARK_$((6*7)); tty; stty size\n');
  check(`${label}: command output`, await until(async () => output().includes('MARK_42')), await screen());
  check(`${label}: has a TTY`, await until(async () => /\/dev\/(pts\/\d+|tty)/.test(output())), output().slice(-300));
  check(`${label}: screen shows it`, (await screen()).includes('MARK_42'), JSON.stringify(frames.slice(-12)) + (await screen()));
  const size = /(\d+) (\d+)/.exec(output().split('tty').pop() ?? '');
  await page.screenshot({ path: `${shots}terminal-${label}-${side}.png` });
  const before = frames.filter((f) => f[1].includes('"resize"')).length;
  await page.setViewportSize({ width: 1100, height: 700 });
  await until(async () => frames.filter((f) => f[1].includes('"resize"')).length > before, 3000);
  const sent = frames.filter((f) => f[1].includes('"resize"')).pop();
  check(`${label}: resize sent`, Boolean(sent));
  if (sent) {
    const { cols, rows } = JSON.parse(sent[1]).resize;
    await page.waitForTimeout(300);
    frames.length = 0;
    await page.keyboard.type('stty size\n');
    check(`${label}: resize reaches the TTY`, await until(async () => output().includes(`${rows} ${cols}`)), `${rows} ${cols} / ${output().slice(-200)}`);
  }
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.waitForTimeout(300);
  await page.locator('button[title="Fullscreen"]').click({ force: true }).catch(() => {});
  await page.waitForTimeout(300);
  check(`${label}: fullscreen`, await page.evaluate(() => document.documentElement.classList.contains('terminal-is-fullscreen')));
  await page.locator('button[title="Exit fullscreen"]').click().catch(() => {});
  await page.waitForTimeout(300);
  await page.locator('#terminal .xterm-helper-textarea').focus().catch(() => {});
  await page.keyboard.type('exit\n');
  check(`${label}: exit ends the session`, await until(() => frames.some((f) => f[1] === 'pty-exited')), output().slice(-300));
  check(`${label}: no command text from the client`, !frames.some((f) => f[0] === '>' && /"(command|ssh)"/.test(f[1])));
  return size;
}

async function open(path) {
  frames = [];
  await page.goto(base + path);
}

const res = `/project/${fx.project}/environment/${fx.environment}`;

if (args.includes('--real-ssh')) {
  // Over real ssh: a server with its shell, a container in Shell mode, a container in Console mode.
  await open(`/server/${fx.real_server}/terminal`);
  await shell('real-ssh-server');
  // /terminal lists containers of every server, and the VM's own row has no route here, so the
  // containers are opened the way that page opens them: token, then the socket.
  const session = (identifier, keys, want) => page.evaluate(async ({ server, identifier, keys, want }) => {
    const { csrf_token } = await (await fetch('/ui/session')).json();
    const r = await fetch('/ui/terminal/session', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': csrf_token },
      body: JSON.stringify({ is_container: true, identifier, server_uuid: server }),
    });
    const answer = await r.json();
    if (!answer.ok) return { answer, seen: '' };
    return new Promise((resolve) => {
      let seen = '';
      const ws = new WebSocket(`ws://${location.host}/terminal/ws`);
      const finish = () => { ws.close(1000); resolve({ answer: { ...answer, token: undefined }, seen }); };
      setTimeout(finish, 15000);
      ws.onopen = () => ws.send(JSON.stringify({ terminalToken: answer.token }));
      ws.onmessage = (e) => {
        seen += e.data;
        if (e.data === 'pty-ready' && keys) setTimeout(() => ws.send(JSON.stringify({ message: keys })), 800);
        if (seen.includes(want)) finish();
      };
    });
  }, { server: fx.real_server, identifier, keys, want });
  const shellDb = await session(fx.database, 'echo MARK_$((6*7)); tty; exit\r', 'MARK_42');
  check('real-ssh container shell', shellDb.seen.includes('MARK_42') && shellDb.answer.mode === 'shell', JSON.stringify(shellDb).slice(0, 400));
  const consoleBox = await session(`sidecar-${fx.service}`, null, 'Ready to accept connections');
  check('real-ssh container console', consoleBox.answer.mode === 'attach' && consoleBox.seen.includes('Ready to accept connections'), JSON.stringify(consoleBox).slice(0, 400));
  await page.waitForTimeout(2500);
  const alive = await new Promise((resolve) => {
    import('node:child_process').then(({ execFile }) => execFile('orb', ['-m', 'coolify-rs-e2e', '-u', 'root', 'docker', 'inspect', '--format', '{{.State.Status}}', `sidecar-${fx.service}`], (e, out) => resolve(String(out).trim())));
  });
  check('real-ssh console: closing the socket detaches, the container keeps running', alive === 'running', alive);
  check('no page errors', errors.length === 0, errors.join(' | '));
  console.log(`${pass}/${pass + fail} passed`);
  await browser.close();
  process.exit(fail ? 1 : 0);
}

await open(`/server/${fx.server}/terminal`);
await shell('server');

await open(`${res}/application/${fx.application}/terminal`);
await shell('application');

await open(`${res}/database/${fx.database}/terminal`);
await shell('database');

await open(`${res}/service/${fx.service}/terminal`);
check('service: picker with both containers', await until(async () => (await page.locator('[data-terminal-target-picker=launcher] button').count()) === 2));
await page.screenshot({ path: `${shots}terminal-service-picker-${side}.png` });
await page.locator('[data-terminal-target-picker=launcher] button', { hasText: 'web-' }).click();
await shell('service');

// Console mode: the sidecar keeps stdin open, so it opens attached, and the switch reaches Shell.
await open(`${res}/service/${fx.service}/terminal`);
await until(async () => (await page.locator('[data-terminal-target-picker=launcher] button').count()) === 2);
await page.locator('[data-terminal-target-picker=launcher] button', { hasText: 'sidecar-' }).click();
check('console: pty-ready', await until(() => frames.some((f) => f[1] === 'pty-ready')));
check('console: mode selector', await until(async () => (await page.getByRole('button', { name: 'Choose terminal mode' }).innerText()).includes('Console')));
check('console: command box', await page.locator('input[aria-label="Console command"]').isVisible());
check('console: history printed', await until(async () => /Ready to accept connections/.test(output())), output().slice(-200));
await page.screenshot({ path: `${shots}terminal-console-${side}.png` });
await page.getByRole('button', { name: 'Choose terminal mode' }).click();
frames.length = 0;
await page.getByRole('menuitemradio').filter({ hasText: 'Shell' }).click();
check('console: switch to shell', await until(() => frames.some((f) => f[1] === 'pty-ready')), JSON.stringify(frames.slice(0, 6)));
await page.waitForTimeout(800);
await page.keyboard.type('echo MARK_$((6*7))\n');
check('console: shell after the switch', await until(async () => output().includes('MARK_42')), output().slice(-300));
await page.keyboard.type('exit\n');
await until(() => frames.some((f) => f[1] === 'pty-exited'));

// /terminal: a server and a container from the target list.
await open('/terminal');
check('index: targets', await until(async () => (await page.locator('.command-palette-item').count()) > 0));
await page.screenshot({ path: `${shots}terminal-index-${side}.png` });
await page.locator('.command-palette-item').first().click();
await shell('index-server');
await open('/terminal');
await until(async () => (await page.locator('.command-palette-item', { hasText: fx.database }).count()) > 0);
await page.locator('.command-palette-item', { hasText: fx.database }).click();
await shell('index-container');

if (side === 'rs') check('no page errors', errors.length === 0, errors.join(' | '));
else console.log('reference page errors:', errors.join(' | '));
console.log(`${pass}/${pass + fail} passed`);
await browser.close();
process.exit(fail ? 1 : 0);
