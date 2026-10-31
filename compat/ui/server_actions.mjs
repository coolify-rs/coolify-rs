// Runs the actions that change a server through both UIs, one throwaway VM per side, and compares toasts,
// rows, containers, files and the commands the VMs received.
//   node server_actions.mjs --rs=18087 --vm-a=coolify-rs-agent-a --vm-b=coolify-rs-agent-b [--only=proxy,images] [--keep] [--servers=refuuid,rsuuid]
// The VMs need sshd with the "orbstack e2e" key. --keep leaves the two servers for the next run (--servers=).
// Evidence per step: shots/actions/<step>.json and <step>-{ref,rs}.png.
import { chromium } from 'playwright-core';
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';

const argv = process.argv.slice(2);
const opt = (k, d) => (argv.find((a) => a.startsWith(`--${k}=`)) ?? `=${d}`).split('=').slice(1).join('=');
const flag = (k) => argv.includes(`--${k}`);
const rsPort = opt('rs', 18087);
const VM = { ref: opt('vm-a', 'coolify-rs-agent-a'), rs: opt('vm-b', 'coolify-rs-agent-b') };
const only = opt('only', '') ? opt('only', '').split(',') : null;
const BASE = { ref: 'http://127.0.0.1:18000', rs: `http://127.0.0.1:${rsPort}` };
const KEY_UUID = 'hv41morfjv9x0av5kakzdmv8';
const secrets = new URL('../reference/secrets/', import.meta.url);
const password = fs.readFileSync(new URL('ui-login.txt', secrets), 'utf8').match(/^password:\s*(.+)$/m)[1].trim();
const TOKEN = JSON.parse(fs.readFileSync(new URL('tokens.json', secrets), 'utf8')).acme_owner_root;
const out = new URL('./shots/actions/', import.meta.url).pathname;
fs.mkdirSync(out, { recursive: true });
const RS_SSH_LOG_DIR = `${os.homedir()}/.cache/coolify-rs-dev/ssh-log`;
fs.mkdirSync(RS_SSH_LOG_DIR, { recursive: true });

const sql = (q) => execFileSync('docker', ['exec', 'coolify-rs-reference-postgres-1', 'psql', '-U', 'coolify', '-d', 'coolify', '-At', '-c', q], { maxBuffer: 1 << 26 }).toString().trim();
const vm = (side, ...cmd) => {
  try {
    return execFileSync('orb', ['-m', VM[side], '-u', 'root', ...cmd], { stdio: ['ignore', 'pipe', 'ignore'], maxBuffer: 1 << 26, timeout: 60000, killSignal: 'SIGKILL' }).toString();
  } catch (e) {
    return e.stdout?.toString() ?? '';
  }
};
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
async function api(side, method, path, body) {
  const r = await fetch(BASE[side] + path, {
    method,
    headers: { Authorization: `Bearer ${TOKEN}`, Accept: 'application/json', 'Content-Type': 'application/json' },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  return { status: r.status, body: await r.text() };
}

// ── Commands ───────────────────────────────────────────────────────────
// Sent to a VM: the reference through sshd (a ForceCommand logs the script), coolify-rs through
// scripts/ssh-via-orb.sh, which logs when its directory exists.
const CMD_LOGGER = `#!/bin/bash
LOG=/var/log/coolify-cmds.log
if [[ "$SSH_ORIGINAL_COMMAND" == *sftp-server* || "$SSH_ORIGINAL_COMMAND" == scp* ]]; then
  echo "#### $SSH_ORIGINAL_COMMAND" >> $LOG
  exec bash -c "$SSH_ORIGINAL_COMMAND"
fi
if [[ "$SSH_ORIGINAL_COMMAND" == "bash -se" || "$SSH_ORIGINAL_COMMAND" == *"exec bash -se"* ]]; then
  t=$(mktemp); cat > "$t"; { echo "####"; cat "$t"; echo; } >> $LOG
  bash -se < "$t"; rc=$?; rm -f "$t"; exit $rc
fi
echo "#### $SSH_ORIGINAL_COMMAND" >> $LOG
exec bash -c "$SSH_ORIGINAL_COMMAND"
`;
function installCommandLog() {
  const file = `${os.tmpdir()}/coolify-cmdlog-${process.pid}`;
  fs.writeFileSync(file, CMD_LOGGER);
  execFileSync('orb', ['-m', VM.ref, '-u', 'root', 'tee', '/usr/local/sbin/coolify-cmdlog'], { input: CMD_LOGGER, stdio: ['pipe', 'ignore', 'ignore'] });
  vm('ref', 'chmod', '755', '/usr/local/sbin/coolify-cmdlog');
  const conf = 'ForceCommand /usr/local/sbin/coolify-cmdlog\n';
  execFileSync('orb', ['-m', VM.ref, '-u', 'root', 'tee', '/etc/ssh/sshd_config.d/10-cmdlog.conf'], { input: conf, stdio: ['pipe', 'ignore', 'ignore'] });
  vm('ref', 'systemctl', 'restart', 'ssh');
  // A multiplexed connection of the reference keeps the old configuration.
  vm('ref', 'pkill', '-f', 'sshd-session');
  fs.rmSync(file, { force: true });
}
const rsLogFile = () => `${RS_SSH_LOG_DIR}/${VM.rs}.orb.local`;
function drainCommands(side, names) {
  let text;
  if (side === 'ref') {
    text = vm('ref', 'cat', '/var/log/coolify-cmds.log');
    vm('ref', 'truncate', '-s', '0', '/var/log/coolify-cmds.log');
  } else {
    text = fs.existsSync(rsLogFile()) ? fs.readFileSync(rsLogFile(), 'utf8') : '';
    fs.writeFileSync(rsLogFile(), '');
  }
  return normCommands(text, names);
}
function normalize(text, names) {
  let t = text;
  for (const [k, v] of Object.entries(names).sort((a, b) => String(b[1]).length - String(a[1]).length)) {
    if (String(v).length >= 3) t = t.split(String(v)).join(`<${k}>`);
  }
  return t
    .replace(/\d{4}-\d\d-\d\d[T ]\d\d:\d\d:\d\d(\.\d+)?Z?/g, '<ts>')
    .replace(/TOKEN=eyJ[A-Za-z0-9+\/=]+/g, 'TOKEN=<encrypted>')
    .replace(/Bearer eyJ[A-Za-z0-9+\/=]+/g, 'Bearer <encrypted>')
    .replace(/backup-\d{8}T\d{6}Z-[0-9a-f]{8}/g, 'backup-<stamp>')
    .replace(/backup-[0-9a-f]{8}\.tmp/g, 'backup-<hex>.tmp')
    .replace(/\b\d{10,}\b/g, '<n>')
    .replace(/\b[a-z0-9]{24}\b/g, '<uuid>')
    .replace(/\b[a-f0-9]{12,64}\b/g, '<hex>');
}
function normCommands(text, names) {
  const lines = normalize(text, names).split('\n').map((l) => l.trim()).filter((l) => l && !l.startsWith('####') && l !== 'echo "health_check_ok"');
  return lines;
}
// Scheduled checks of the reference (`ServerManagerJob`), not a result of a step.
// coolify-rs uploads acme.json through the shell where upstream uses scp (docs/KNOWN_LIMITATIONS.md).
const NOISE = /coolify-acme-<hex>\.json'$|^docker inspect .*coolify-proxy|docker ps|docker container|df -|echo 'Alive|echo "Alive|ls \/ \|\||coolify-sentinel --format|ls \/|^set -e|^PATH=|owner\.json|^docker version --format json|^docker compose version --short|api\/push-status|^docker logs -n|^mkdir -p \/tmp|coolify-upload|^echo '[A-Za-z0-9+\/=]{200,}/;

// ── Browser ────────────────────────────────────────────────────────────
const browser = await chromium.launch({ executablePath: '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', headless: true });
async function login(base) {
  const ctx = await browser.newContext({ viewport: { width: 1440, height: 1000 }, colorScheme: 'dark' });
  const page = await ctx.newPage();
  await page.addInitScript(() => {
    try {
      localStorage.setItem('theme', 'dark');
    } catch {}
    window.__toasts = [];
    const seen = new WeakSet();
    const scan = (root) => {
      const cards = root.querySelectorAll ? [...root.querySelectorAll('.toast-card'), ...(root.matches?.('.toast-card') ? [root] : [])] : [];
      for (const c of cards) {
        if (seen.has(c)) continue;
        seen.add(c);
        setTimeout(() => window.__toasts.push(c.innerText.replace(/\s+/g, ' ').trim()), 250);
      }
    };
    new MutationObserver((ms) => ms.forEach((m) => m.addedNodes.forEach(scan))).observe(document, { childList: true, subtree: true });
  });
  await page.goto(base + '/login');
  await page.fill('input[name=email]', 'alice@example.test');
  await page.fill('input[name=password]', password);
  await Promise.all([page.waitForURL((u) => !u.pathname.startsWith('/login'), { timeout: 15000 }).catch(() => {}), page.click('button[type=submit]')]);
  await page.locator('button:has-text("Accept And Close"), button:has-text("Accept and Close")').first().click({ timeout: 1500 }).catch(() => {});
  return page;
}
const pages = {};
let server = {};
const toasts = async (side) => {
  await sleep(400);
  return pages[side].evaluate(() => {
    const t = window.__toasts ?? [];
    window.__toasts = [];
    return t;
  }).catch(() => []);
};

/** The topmost visible button with this text: a modal's button rather than the one that opened it. */
async function clickTop(page, text, { within, timeout = 15000 } = {}) {
  const end = Date.now() + timeout;
  const re = text instanceof RegExp ? text : new RegExp(`^\\s*${text.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}\\s*$`);
  const scope = within ?? page;
  while (Date.now() < end) {
    const all = await scope.locator('button, a').filter({ hasText: re }).all();
    for (const el of all.reverse()) {
      if (!(await el.isVisible().catch(() => false))) continue;
      if (await el.isDisabled().catch(() => false)) continue;
      const hit = await el.evaluate((n) => {
        const r = n.getBoundingClientRect();
        const top = document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2);
        return !!top && (n === top || n.contains(top) || top.contains(n));
      }).catch(() => false);
      if (hit) {
        // A Livewire update can replace the element between the check and the click.
        const clicked = await el.click({ timeout: 3000 }).then(() => true, () => false);
        if (clicked) return;
      }
    }
    await sleep(250);
  }
  throw new Error(`no clickable button ${text}`);
}
async function confirm(page, step2, { typed, step3 } = {}) {
  if (typed) {
    const input = page.locator('input[x-model="userConfirmationText"]:visible, input#userConfirmationText:visible').last();
    await input.waitFor({ timeout: 10000 });
    await input.fill(typed);
    await sleep(200);
  }
  await clickTop(page, step2);
  await sleep(400);
  const pw = page.locator('input[type=password][x-model="password"]:visible, input#password-confirm:visible').last();
  if (await pw.isVisible().catch(() => false)) {
    await pw.fill(password);
    await sleep(200);
    await clickTop(page, step3 ?? 'Confirm');
  }
}
async function listbox(page, id, label) {
  await page.locator(`[id="${id}-trigger"], [id="${id}"]`).first().click();
  await sleep(250);
  await page.locator('.listbox-option:visible').filter({ hasText: label }).first().click();
  await sleep(500);
}
/** A field by its Livewire property: upstream has `wire:model` and a generated id, coolify-rs the property as id. */
const field = (page, id) => page.locator(['id', 'name', 'wire\\:model', 'wire\\:model\\.defer', 'wire\\:model\\.live', 'wire\\:model\\.blur', 'wire\\:model\\.live\\.debounce\\.500ms'].flatMap((a) => [`input[${a}="${id}"]:visible`, `textarea[${a}="${id}"]:visible`]).join(', ')).last();
const open = async (side, path) => {
  const page = pages[side];
  await page.goto(BASE[side] + path.replace('{U}', server[side].uuid).replace('{D}', server[side].dest ?? ''));
  await page.waitForLoadState('networkidle', { timeout: 8000 }).catch(() => {});
  await sleep(1200);
  // Upstream's popups (real-time service, sponsors) cover buttons at the bottom of the page.
  await page.evaluate(() => {
    for (const d of document.querySelectorAll('div')) {
      if (getComputedStyle(d).position === 'fixed' && /Cannot connect to real-time service|Love Coolify/.test(d.innerText ?? '')) d.remove();
    }
  }).catch(() => {});
  return page;
};

// ── State ──────────────────────────────────────────────────────────────
const IGNORE_SERVER = new Set(['id', 'uuid', 'name', 'ip', 'ip_previous', 'created_at', 'updated_at', 'sentinel_updated_at', 'description', 'server_metadata', 'traefik_outdated_info', 'detected_traefik_version', 'hetzner_server_status']);
const IGNORE_SETTINGS = new Set(['id', 'server_id', 'created_at', 'updated_at', 'sentinel_token', 'sentinel_custom_url', 'docker_version_checked_at', 'compose_version_checked_at', 'patch_check_result', 'patch_check_error', 'patch_checked_at']);
function json(q) {
  const r = sql(q);
  return r ? JSON.parse(r) : null;
}
function rows(side) {
  const s = server[side];
  const srv = json(`select row_to_json(t)::text from servers t where id=${s.id}`) ?? {};
  const settings = json(`select row_to_json(t)::text from server_settings t where server_id=${s.id}`) ?? {};
  const proxy = typeof srv.proxy === 'string' ? JSON.parse(srv.proxy || '{}') : srv.proxy ?? {};
  const out = { proxy: { ...proxy } };
  for (const k of ['last_saved_settings', 'last_applied_settings', 'last_saved_proxy_configuration']) if (out.proxy[k]) out.proxy[k] = normalize(String(out.proxy[k]), s.names);
  for (const [k, v] of Object.entries(srv)) if (!IGNORE_SERVER.has(k) && k !== 'proxy') out[`server.${k}`] = v;
  // `StartSentinel` runs asynchronously on each side: a time, or none.
  out['server.sentinel_waiting_since'] = out['server.sentinel_waiting_since'] != null;
  for (const [k, v] of Object.entries(settings)) if (!IGNORE_SETTINGS.has(k)) out[`settings.${k}`] = v;
  out.destinations = sql(`select name,network from standalone_dockers where server_id=${s.id} order by network`).split('\n').map((l) => normalize(l, s.names));
  out.ssl = sql(`select is_ca_certificate,common_name,subject_alternative_names from ssl_certificates where server_id=${s.id} order by id`).split('\n').map((l) => normalize(l, s.names));
  out.cleanup = sql(`select status from docker_cleanup_executions where server_id=${s.id} order by id`).split('\n');
  return out;
}
// One call for all files: an orb process per file hung now and then.
const FILES = ['/etc/docker/daemon.json', '/data/coolify/proxy/docker-compose.yml', '/data/coolify/proxy/caddy/docker-compose.yml',
  '/data/coolify/proxy/dynamic/default_redirect_503.yaml', '/data/coolify/proxy/caddy/dynamic/default_redirect_503.caddy',
  '/data/coolify/log-drains/docker-compose.yml', '/data/coolify/log-drains/.env', '/data/coolify/log-drains/fluent-bit.conf', '/root/.docker/config.json'];
function vmState(side) {
  const names = server[side].names;
  // Only what the steps pull or delete: the rest depends on what ran on the VM before.
  const ps = vm(side, 'docker', 'ps', '-a', '--format', '{{.Names}} {{.Image}} {{.State}}').split('\n').filter(Boolean).sort();
  const nets = vm(side, 'docker', 'network', 'ls', '--format', '{{.Name}}').split('\n').filter(Boolean).sort();
  const images = vm(side, 'docker', 'images', '--format', '{{.Repository}}:{{.Tag}}').split('\n').filter((i) => i.startsWith('hello-world')).sort();
  const files = {};
  const all = vm(side, 'python3', '-c', `import json,sys
out={}
for f in sys.argv[1:]:
    try: out[f]=open(f,errors='replace').read()
    except OSError: out[f]=''
print(json.dumps(out))`, ...FILES);
  const read = all ? JSON.parse(all) : {};
  for (const f of FILES) files[f] = normalize(read[f] ?? '', names);
  const ca = vm(side, 'openssl', 'x509', '-in', '/data/coolify/ssl/coolify-ca.crt', '-noout', '-subject', '-issuer');
  return { ps, nets, images, files, ca };
}
function diff(a, b, path = '', outp = []) {
  if (JSON.stringify(a) === JSON.stringify(b)) return outp;
  if (a && b && typeof a === 'object' && typeof b === 'object' && !Array.isArray(a) && !Array.isArray(b)) {
    for (const k of new Set([...Object.keys(a), ...Object.keys(b)])) diff(a[k], b[k], path ? `${path}.${k}` : k, outp);
  } else {
    outp.push(`${path}: ref ${JSON.stringify(a)?.slice(0, 500)} | rs ${JSON.stringify(b)?.slice(0, 500)}`);
  }
  return outp;
}

let failed = 0;
const report = [];
function verdict(name, ok, detail) {
  if (!ok) failed++;
  const line = `${ok ? 'PASS' : 'FAIL'} ${name}${ok ? '' : `\n     ${detail}`}`;
  report.push(line);
  console.log(line);
}

/** One action on both sides, then everything compared. `wait` runs per side until true. */
async function step(name, fn, { wait, settle = 2500, ignore = [], waitMs = 120000 } = {}) {
  const section = name.split(':')[0];
  if (only && !only.includes(section)) return;
  for (const side of ['ref', 'rs']) drainCommands(side, server[side].names);
  const errors = {};
  await Promise.all(['ref', 'rs'].map(async (side) => {
    try {
      await pages[side].evaluate(() => (window.__toasts = [])).catch(() => {});
      await fn(side, pages[side]);
    } catch (e) {
      errors[side] = String(e).slice(0, 300);
      await pages[side].screenshot({ path: `${out}${name.replace(/\W+/g, '_')}-${side}-error.png` }).catch(() => {});
    }
  }));
  await Promise.all(['ref', 'rs'].map(async (side) => {
    if (!wait) return;
    const end = Date.now() + waitMs;
    while (Date.now() < end) {
      if (await wait(side)) return;
      await sleep(3000);
    }
    errors[side] = (errors[side] ?? '') + ' timed out waiting';
  }));
  await sleep(settle);
  const t = { ref: await toasts('ref'), rs: await toasts('rs') };
  for (const side of ['ref', 'rs']) await pages[side].screenshot({ path: `${out}${name.replace(/\W+/g, '_')}-${side}.png` }).catch(() => {});
  const st = { ref: { ...rows('ref'), ...vmState('ref') }, rs: { ...rows('rs'), ...vmState('rs') } };
  const cmds = { ref: drainCommands('ref', server.ref.names), rs: drainCommands('rs', server.rs.names) };
  const setA = new Set(cmds.ref.filter((c) => !NOISE.test(c)));
  const setB = new Set(cmds.rs.filter((c) => !NOISE.test(c)));
  const onlyRef = [...setA].filter((c) => !setB.has(c));
  const onlyRs = [...setB].filter((c) => !setA.has(c));
  const stDiff = diff(st.ref, st.rs).filter((d) => !ignore.some((i) => d.startsWith(i)));
  fs.writeFileSync(`${out}${name.replace(/\W+/g, '_')}.json`, JSON.stringify({ name, errors, toasts: t, stDiff, onlyRef, onlyRs, cmds, st }, null, 1));
  verdict(`${name} ran`, !errors.ref && !errors.rs, JSON.stringify(errors));
  verdict(`${name} toasts`, JSON.stringify(t.ref) === JSON.stringify(t.rs), `ref ${JSON.stringify(t.ref)}\n     rs  ${JSON.stringify(t.rs)}`);
  verdict(`${name} state`, stDiff.length === 0, stDiff.join('\n     '));
  verdict(`${name} commands`, onlyRef.length === 0 && onlyRs.length === 0, `only ref ${JSON.stringify(onlyRef).slice(0, 2000)}\n     only rs ${JSON.stringify(onlyRs).slice(0, 2000)}`);
}

const proxyOf = (side) => json(`select proxy::text from servers where id=${server[side].id}`) ?? {};
const proxyIs = (status) => (side) => proxyOf(side).status === status;

// Sections with their own setup outside `step()`: skipped unless asked for by --only.
const NEW_SECTIONS = ['patches', 'patches-reboot', 'patches-all', 'tunnel', 'runners', 'analytics'];
const want = (x) => only?.includes(x);
const settings = (side, col) => sql(`select ${col} from server_settings where server_id=${server[side].id}`);
/** The saved patch check of one side, without what differs per VM. */
const patchResult = (side) => normalize(settings(side, 'patch_check_result::text'), server[side].names);
// The check runs on a queue of the reference, which can be busy: done when the saved time moved.
const checkedAt = {};
const markChecked = () => {
  for (const side of ['ref', 'rs']) checkedAt[side] = settings(side, 'patch_checked_at');
};
const patchesDone = (side) => settings(side, 'patch_checked_at') !== checkedAt[side];

// ── Setup ──────────────────────────────────────────────────────────────
async function createServers() {
  if (opt('servers', '')) {
    const [a, b] = opt('servers', '').split(',');
    return { ref: a, rs: b };
  }
  const made = {};
  for (const [side, name] of [['ref', 'diff-ui-a'], ['rs', 'diff-ui-b']]) {
    const r = await api('ref', 'POST', '/api/v1/servers', { name, ip: `${VM[side]}.orb.local`, private_key_uuid: KEY_UUID, proxy_type: 'traefik' });
    if (r.status !== 201) throw new Error(`create ${r.status} ${r.body}`);
    made[side] = JSON.parse(r.body).uuid;
  }
  return made;
}
async function deleteServers() {
  for (const side of ['ref', 'rs']) {
    const sid = server[side].id;
    for (const q of [`delete from docker_cleanup_executions where server_id=${sid}`, `delete from shared_environment_variables where server_id=${sid}`,
      `delete from standalone_dockers where server_id=${sid}`, `delete from swarm_dockers where server_id=${sid}`, `delete from ssl_certificates where server_id=${sid}`,
      `delete from server_settings where server_id=${sid}`, `delete from servers where id=${sid}`]) {
      sql(q);
    }
  }
}
const uuids = await createServers();
for (const side of ['ref', 'rs']) {
  const id = sql(`select id from servers where uuid='${uuids[side]}'`);
  const name = sql(`select name from servers where id=${id}`);
  server[side] = { uuid: uuids[side], id, names: { U: uuids[side], SERVER: name, VM: `${VM[side]}.orb.local`, VMNAME: VM[side] } };
}
if (!opt('servers', '')) {
  await api('ref', 'POST', `/api/v1/servers/${uuids.ref}/validate`, { install: true });
  await api('rs', 'POST', `/api/v1/servers/${uuids.rs}/validate`, { install: true });
  for (const side of ['ref', 'rs']) {
    const end = Date.now() + 600000;
    while (Date.now() < end && !(sql(`select is_usable and is_reachable from server_settings where server_id=${server[side].id}`) === 't' && proxyOf(side).status === 'running')) await sleep(5000);
    console.log(side, 'server ready', proxyOf(side).status);
  }
}
// A previous run may have stopped halfway.
for (const side of ['ref', 'rs']) {
  const sid = server[side].id;
  sql(`delete from standalone_dockers where server_id=${sid} and network <> 'coolify'`);
  sql(`update server_settings set logdrain_axiom_dataset_name=null, logdrain_axiom_api_key=null, is_logdrain_axiom_enabled=false where server_id=${sid}`);
  for (const n of ['diff-scan', 'diff-created-net']) vm(side, 'docker', 'network', 'rm', n);
  for (const c of ['diff-unmanaged', 'diff-registry']) vm(side, 'docker', 'rm', '-f', c);
  vm(side, 'rm', '-rf', '/data/coolify/log-drains', '/root/.docker/config.json');
}
installCommandLog();
fs.writeFileSync(rsLogFile(), '');
pages.ref = await login(BASE.ref);
pages.rs = await login(BASE.rs);

try {
  await runAll();
} finally {
  await browser.close();
  vm('ref', 'rm', '-f', '/etc/ssh/sshd_config.d/10-cmdlog.conf');
  vm('ref', 'systemctl', 'restart', 'ssh');
  fs.rmSync(rsLogFile(), { force: true });
  if (!flag('keep')) await deleteServers();
  else console.log(`kept: --servers=${uuids.ref},${uuids.rs}`);
}
console.log(`${report.filter((l) => l.startsWith('PASS')).length}/${report.length} checks passed`);
process.exit(failed ? 1 : 0);

// ── Steps ──────────────────────────────────────────────────────────────
async function runNewSections() {
  if (want('patches')) await runPatches();
  if (want('patches-reboot')) await runPatchesReboot();
  if (want('patches-all')) await runPatchesAll();
  if (want('tunnel')) await runTunnel();
  if (want('runners')) await runRunners();
}
async function compareActivityLog(name) {
  const logs = {};
  for (const side of ['ref', 'rs']) {
    const text = await pages[side].locator('.activity-monitor-log').last().innerText({ timeout: 5000 }).catch((e) => `no log: ${e}`);
    logs[side] = normalize(text, server[side].names).split('\n').slice(0, 3).join('\n');
  }
  verdict(`${name} log start`, logs.ref === logs.rs, `ref ${logs.ref}\n     rs  ${logs.rs}`);
}
async function runPatches() {
  const P = '/server/{U}/security/patches';
  const save = async (side, freq, time) => {
    const page = await open(side, P);
    await field(page, 'patchCheckFrequency').fill(freq);
    if (time !== undefined) await field(page, 'autoSecurityUpdatesTime').fill(time);
    await sleep(600);
    await page.keyboard.press('Enter');
    await sleep(1500);
  };
  await step('patches: reject a frequency above once per hour', (side) => save(side, '*/5 * * * *'));
  await step('patches: reject an invalid expression', (side) => save(side, 'nonsense'));
  await step('patches: save a frequency', (side) => save(side, '0 3 * * 1'));
  await step('patches: clear the frequency', (side) => save(side, ''));
  markChecked();
  await step('patches: check for updates', async (side) => {
    const page = await open(side, P);
    await clickTop(page, /^\s*Check for updates\s*$/);
    await sleep(2000);
  }, { wait: patchesDone, settle: 3000, waitMs: 400000 });
  verdict('patches: saved result', patchResult('ref') === patchResult('rs'), `ref ${patchResult('ref').slice(0, 1500)}\n     rs  ${patchResult('rs').slice(0, 1500)}`);
  verdict('patches: saved error', settings('ref', 'patch_check_error') === settings('rs', 'patch_check_error'), `${settings('ref', 'patch_check_error')} | ${settings('rs', 'patch_check_error')}`);
  await step('patches: reject an invalid time', async (side) => {
    const page = await open(side, P);
    await field(page, 'autoSecurityUpdatesTime').fill('25:99');
    await sleep(600);
    await page.keyboard.press('Enter');
    await sleep(1500);
  });
  await step('patches: turn on automatic updates', async (side) => {
    const page = await open(side, P);
    await clickTop(page, 'Turn on');
    await confirm(page, 'Turn on');
    await sleep(25000);
  }, { settle: 3000 });
  const autoFiles = (side) => vm(side, 'sh', '-c', 'cat /etc/apt/apt.conf.d/52coolify-unattended-upgrades /etc/systemd/system/apt-daily-upgrade.timer.d/coolify.conf; systemctl is-enabled apt-daily-upgrade.timer');
  verdict('patches: automatic updates files', autoFiles('ref') === autoFiles('rs') && autoFiles('rs').includes('Managed by Coolify'), `ref ${autoFiles('ref')}\n     rs ${autoFiles('rs')}`);
  await step('patches: change the maintenance window while on', (side) => save(side, '', '04:30'), { settle: 4000 });
  verdict('patches: new window on the VM', autoFiles('ref') === autoFiles('rs') && autoFiles('rs').includes('04:30'), `ref ${autoFiles('ref')}\n     rs ${autoFiles('rs')}`);
  await step('patches: turn off automatic updates', async (side) => {
    const page = await open(side, P);
    await clickTop(page, 'Turn off');
    await confirm(page, 'Turn off');
    await sleep(5000);
  }, { settle: 3000 });
  verdict('patches: automatic updates files removed', !autoFiles('ref').includes('Managed') && !autoFiles('rs').includes('Managed'), autoFiles('rs'));
  await step('patches: restore the maintenance window', (side) => save(side, '', '03:00'));
  await step('patches: update one package', async (side) => {
    const page = await open(side, P);
    await page.locator('.data-table-row').filter({ hasText: 'ubuntu-pro-client-l10n' }).getByRole('button', { name: 'Update', exact: true }).click();
    await sleep(25000);
  }, { settle: 2000 });
  await compareActivityLog('patches: update one package');
  for (const side of ['ref', 'rs']) await pages[side].screenshot({ path: `${out}patches_dialog-${side}.png` });
  const version = (side) => vm(side, 'dpkg-query', '-W', 'ubuntu-pro-client-l10n');
  verdict('patches: package updated', version('ref') === version('rs'), `${version('ref')} | ${version('rs')}`);
  // Stays out of the state comparison: the check that the update starts ends at another moment on each side.
  await sleep(60000);
  verdict('patches: result after the update', patchResult('ref') === patchResult('rs'), `ref ${patchResult('ref').slice(0, 600)}\n     rs  ${patchResult('rs').slice(0, 600)}`);
}
async function runPatchesReboot() {
  const P = '/server/{U}/security/patches';
  for (const side of ['ref', 'rs']) vm(side, 'touch', '/var/run/reboot-required');
  markChecked();
  await step('patches-reboot: check shows reboot required', async (side) => {
    const page = await open(side, P);
    await clickTop(page, /^\s*Check for updates\s*$/);
    await sleep(2000);
  }, { wait: patchesDone, settle: 3000, waitMs: 400000 });
  verdict('patches-reboot: result', patchResult('ref') === patchResult('rs') && patchResult('rs').includes('"reboot_required":true'), patchResult('rs').slice(0, 500));
  await step('patches-reboot: reboot', async (side) => {
    const page = await open(side, P);
    await page.screenshot({ path: `${out}patches_reboot_ready-${side}.png` });
    await clickTop(page, 'Reboot server');
    await confirm(page, 'Reboot server', { typed: server[side].names.SERVER });
    await sleep(3000);
  }, { settle: 1000, ignore: ['ps', 'nets', 'files', 'ca', 'settings.', 'server.', 'proxy', 'destinations', 'ssl', 'cleanup', 'images'] });
}
async function runTunnel() {
  const P = '/server/{U}/cloudflare-tunnel';
  const pass = async (page) => {
    await sleep(300);
    const pw = page.locator('input[type=password]:visible').last();
    if (await pw.isVisible().catch(() => false)) {
      await pw.fill(password);
      await sleep(200);
      await clickTop(page, 'Confirm');
    }
  };
  const shoot = async (name) => {
    for (const side of ['ref', 'rs']) await pages[side].screenshot({ path: `${out}tunnel_${name}-${side}.png` });
  };
  // No token, then a token of this team without and with the capability. Only the name and the capability are read.
  const team = sql(`select team_id from servers where id=${server.ref.id}`);
  await step('tunnel: no token', (side) => open(side, P));
  await shoot('no_token');
  const insert = (caps) => sql(`insert into integration_tokens (uuid, team_id, name, provider, token, capabilities, created_at, updated_at) values ('diff-tunnel-token', ${team}, 'diff tunnel token', 'cloudflare', 'unused', '${caps}', now(), now()) on conflict (uuid) do update set capabilities=excluded.capabilities`);
  try {
    insert('["dns"]');
    await step('tunnel: token without the capability', (side) => open(side, P));
    await shoot('no_capability');
    insert('["dns","tunnel"]');
    await step('tunnel: empty api form', async (side) => {
      const page = await open(side, P);
      await page.locator('[id="api_ssh_domain"], [wire\\:model="api_ssh_domain"]').first().evaluate((n) => n.closest('form').noValidate = true);
      await clickTop(page, 'Set up tunnel');
      await sleep(1500);
    });
    await shoot('api_empty');
    await step('tunnel: invalid hostname', async (side) => {
      const page = await pages[side];
      await listbox(page, 'cloudflare_integration_token_id', 'diff tunnel token');
      await field(page, 'api_ssh_domain').fill('bad_host name');
      await clickTop(page, 'Set up tunnel');
      await sleep(1500);
    });
    await shoot('api_invalid');
    await step('tunnel: existing tunnel form', async (side) => {
      const page = await open(side, P);
      await clickTop(page, 'Use an existing tunnel with its tunnel token');
      await sleep(400);
    });
    await shoot('collapsible');
  } finally {
    sql(`delete from integration_tokens where uuid='diff-tunnel-token'`);
  }
  await step('tunnel: manual configuration', async (side) => {
    const page = await open(side, P);
    await clickTop(page, 'I configured the tunnel manually');
    await sleep(300);
    await page.locator('input[x-model="userConfirmationText"]:visible, input#userConfirmationText:visible').last().fill('I manually configured Cloudflare Tunnel');
    await sleep(200);
    await clickTop(page, /^\s*(Continue|Confirm)\s*$/);
    await pass(page);
    await sleep(2000);
  });
  await shoot('enabled');
  await step('tunnel: update without a configuration', async (side) => {
    const page = await open(side, P);
    await clickTop(page, 'Update Cloudflare Tunnel');
    await sleep(3000);
  });
  // A stub Compose file instead of a tunnel: the update path without Cloudflare.
  for (const side of ['ref', 'rs']) {
    vm(side, 'mkdir', '-p', '/data/coolify/cloudflared');
    execFileSync('orb', ['-m', VM[side], '-u', 'root', 'tee', '/data/coolify/cloudflared/docker-compose.yml'], { input: 'services:\n  noop:\n    image: hello-world\n', stdio: ['pipe', 'ignore', 'ignore'] });
  }
  await step('tunnel: update with a stub configuration', async (side) => {
    const page = await open(side, P);
    await clickTop(page, 'Update Cloudflare Tunnel');
    await sleep(25000);
  }, { settle: 3000 });
  await compareActivityLog('tunnel: update');
  await shoot('update_dialog');
  for (const side of ['ref', 'rs']) {
    vm(side, 'docker', 'rm', '-f', 'cloudflared-noop-1');
    vm(side, 'docker', 'network', 'rm', 'cloudflared_default');
    vm(side, 'docker', 'rmi', 'hello-world');
    vm(side, 'rm', '-rf', '/data/coolify/cloudflared');
  }
  await step('tunnel: disable', async (side) => {
    const page = await open(side, P);
    await clickTop(page, 'Disable Cloudflare Tunnel');
    await sleep(300);
    await page.locator('input[x-model="userConfirmationText"]:visible, input#userConfirmationText:visible').last().fill('DISABLE CLOUDFLARE TUNNEL');
    await sleep(200);
    await clickTop(page, /^\s*(Continue|Confirm)\s*$/);
    await pass(page);
    await sleep(4000);
  });
  await shoot('disabled');
  for (const side of ['ref', 'rs']) sql(`update server_settings set is_usable=false where server_id=${server[side].id}`);
  await step('tunnel: not validated', (side) => open(side, P), { ignore: ['settings.is_usable', 'settings.is_reachable'] });
  await shoot('not_validated');
  for (const side of ['ref', 'rs']) sql(`update server_settings set is_usable=true where server_id=${server[side].id}`);
}
function runnerConfig(side) {
  const r = json(`select row_to_json(t)::text from (select labels::text, is_enabled, is_dedicated, max_runners, docker_mode, runner_image, cpu_limit, memory_limit, capacity_wait_timeout, idle_timeout, job_timeout, allow_pull_requests from github_runner_configs where server_id=${server[side].id}) t`);
  return JSON.stringify(r);
}
async function runRunners() {
  const P = '/server/{U}/github-runners';
  const shoot = async (name) => {
    for (const side of ['ref', 'rs']) await pages[side].screenshot({ path: `${out}runners_${name}-${side}.png`, fullPage: true });
  };
  const team = sql(`select team_id from servers where id=${server.ref.id}`);
  const compareConfig = (name) => verdict(`runners: ${name} config row`, runnerConfig('ref') === runnerConfig('rs'), `ref ${runnerConfig('ref')}\n     rs  ${runnerConfig('rs')}`);
  const fill = async (side, values) => {
    const page = pages[side];
    for (const [k, v] of Object.entries(values)) await field(page, k).fill(v);
    await sleep(500);
    await page.keyboard.press('Enter');
    await sleep(2000);
  };
  await step('runners: no organization app', (side) => open(side, P));
  await shoot('no_app');
  // A GitHub App that is not ready, with its API on a closed local port: nothing can reach GitHub.
  sql(`insert into github_apps (uuid, name, organization, api_url, html_url, app_id, installation_id, team_id, created_at, updated_at) values ('diff-runner-app', 'diff runner app', 'diff-org', 'http://127.0.0.1:9', 'https://github.example.test', 1, 1, ${team}, now(), now())`);
  const appId = sql(`select id from github_apps where uuid='diff-runner-app'`);
  const execs = [];
  try {
    await step('runners: disabled', (side) => open(side, P));
    await shoot('disabled');
    await step('runners: enable', async (side) => {
      const page = await open(side, P);
      await clickTop(page, 'Enable runners');
      await sleep(500);
      await clickTop(page, 'Enable runners');
      await sleep(3000);
    });
    compareConfig('enabled');
    await shoot('enabled');
    await step('runners: reject values', async (side) => {
      await open(side, P);
      await fill(side, { maxRunners: '99', cpuLimit: 'abc', memoryLimit: '4x', runnerImage: 'Bad Image', capacityWaitTimeout: '0' });
    });
    await shoot('rejected');
    await step('runners: reject reserved labels', async (side) => {
      await open(side, P);
      await fill(side, { labels: 'linux, x64' });
    });
    await shoot('labels');
    await step('runners: save', async (side) => {
      await open(side, P);
      await fill(side, { labels: 'Build, build, linux, gpu_1', maxRunners: '4', cpuLimit: '1.5', memoryLimit: '4G', runnerImage: 'ghcr.io/actions/actions-runner:2.337.0', capacityWaitTimeout: '30', idleTimeout: '5', jobTimeout: '120' });
    });
    compareConfig('saved');
    await step('runners: save listboxes', async (side) => {
      const page = await open(side, P);
      await listbox(page, 'allowPullRequests', 'Run pull request jobs');
      await listbox(page, 'dockerMode', 'Docker (privileged, full host access)');
      await sleep(500);
      await page.locator('input:visible').first().focus();
      await page.keyboard.press('Enter');
      await sleep(2000);
    });
    compareConfig('listboxes');
    await shoot('warnings');
    await step('runners: sysbox missing', async (side) => {
      const page = await open(side, P);
      await listbox(page, 'dockerMode', 'Isolated Docker (Sysbox)');
      await sleep(4000);
      await field(page, 'cpuLimit').fill('2');
      await sleep(600);
      await page.keyboard.press('Enter');
      await sleep(4000);
    });
    await shoot('sysbox');
    // Executions: rows only; the cancel cleanup runs real commands for a container that does not exist.
    for (const side of ['ref', 'rs']) {
      server[side].names.EXEC = `diff-exec-${side}`;
      const id = sql(`insert into github_runner_executions (trigger_workflow_job_id, uuid, github_app_id, github_runner_config_id, server_id, status, repository_full_name, workflow_name, job_name, created_at, updated_at) select ${side === 'ref' ? 990001 : 990002}, 'diff-exec-${side}', ${appId}, id, ${server[side].id}, 'idle', 'diff-org/repo', 'CI', 'build', now(), now() from github_runner_configs where server_id=${server[side].id} returning id`).split('\n')[0];
      execs.push(id);
      sql(`insert into github_runner_executions (trigger_workflow_job_id, uuid, github_app_id, github_runner_config_id, server_id, status, repository_full_name, conclusion, started_at, completed_at, workflow_job_html_url, created_at, updated_at) select ${side === 'ref' ? 990003 : 990004}, 'diff-done-${side}', ${appId}, id, ${server[side].id}, 'completed', 'diff-org/repo', 'success', now() - interval '125 seconds', now(), 'https://github.example.test/run', now(), now() from github_runner_configs where server_id=${server[side].id}`);
    }
    await step('runners: executions', (side) => open(side, P));
    await shoot('executions');
    await step('runners: cancel', async (side) => {
      const page = await open(side, P);
      await page.locator('#github-runners-executions-section').scrollIntoViewIfNeeded();
      await sleep(300);
      await clickTop(page, 'Cancel');
      await sleep(400);
      await clickTop(page, 'Cancel Runner');
      await sleep(3000);
    }, { settle: 8000 });
    const finished = (side) => sql(`select status, error_message, completed_at is not null from github_runner_executions where uuid='diff-exec-${side}'`);
    verdict('runners: cancelled row', finished('ref') === finished('rs') && finished('rs').startsWith('cancelled'), `${finished('ref')} | ${finished('rs')}`);
    await shoot('cancelled');
    await step('runners: disable', async (side) => {
      const page = await open(side, P);
      await clickTop(page, 'Disable runners');
      await sleep(2500);
    });
    compareConfig('disabled');
    await step('runners: enable again', async (side) => {
      const page = await open(side, P);
      await clickTop(page, 'Enable runners');
      await sleep(500);
      await clickTop(page, 'Enable runners');
      await sleep(3000);
    });
    compareConfig('enabled again');
  } finally {
    sql(`delete from github_runner_executions where github_app_id=${appId}`);
    for (const side of ['ref', 'rs']) sql(`delete from github_runner_configs where server_id=${server[side].id}`);
    sql(`delete from github_apps where id=${appId}`);
  }
  for (const side of ['ref', 'rs']) sql(`update server_settings set server_role='deployment' where server_id=${server[side].id}`);
  await step('runners: deployment role', (side) => open(side, P), { ignore: ['settings.server_role'] });
  await shoot('deployment');
  for (const side of ['ref', 'rs']) sql(`update server_settings set server_role='both' where server_id=${server[side].id}`);
}
async function runPatchesAll() {
  const P = '/server/{U}/security/patches';
  await step('patches-all: update all packages', async (side) => {
    const page = await open(side, P);
    await clickTop(page, 'Update all packages');
    await confirm(page, 'Update All Packages', { typed: 'Update All Packages' });
    await sleep(90000);
  }, { settle: 2000 });
  await compareActivityLog('patches-all: update all');
  for (const side of ['ref', 'rs']) await pages[side].screenshot({ path: `${out}patches_all_dialog-${side}.png` });
  const list = (side) => vm(side, 'dpkg-query', '-W', '-f', '${Package} ${Version}\n');
  verdict('patches-all: package versions', list('ref') === list('rs'), 'versions differ');
}
async function runAll() {
  await runNewSections();
  if (only && only.every((x) => NEW_SECTIONS.includes(x))) return;
  // Proxy.
  await step('proxy: start', async (side) => {
    if (proxyOf(side).status === 'running') return;
    const page = await open(side, '/server/{U}/proxy');
    await clickTop(page, 'Start Proxy');
  }, { wait: proxyIs('running'), settle: 8000 });
  await step('proxy: stop', async (side) => {
    const page = await open(side, '/server/{U}/proxy');
    await page.locator('.split-action-caret:visible').first().click();
    await clickTop(page, 'Stop Proxy');
    await confirm(page, 'Stop Proxy');
  }, { wait: proxyIs('exited') });
  await step('proxy: switch needs selection', async (side) => {
    const page = await open(side, '/server/{U}/proxy');
    await clickTop(page, 'Switch proxy');
    await confirm(page, 'Switch Proxy');
    await sleep(2500);
  });
  await step('proxy: select caddy', async (side) => {
    const page = await open(side, '/server/{U}/proxy');
    await page.locator('button:visible').filter({ hasText: /^\s*Caddy/ }).first().click();
    await sleep(3000);
  });
  await step('proxy: caddy running', async () => {}, { wait: proxyIs('running'), settle: 5000 });
  await step('proxy: restart', async (side) => {
    const page = await open(side, '/server/{U}/proxy');
    await clickTop(page, 'Restart Proxy');
    await confirm(page, 'Restart Proxy');
    await sleep(5000);
  }, { wait: proxyIs('running'), settle: 10000 });
  await step('proxy: stop caddy', async (side) => {
    const page = await open(side, '/server/{U}/proxy');
    await page.locator('.split-action-caret:visible').first().click();
    await clickTop(page, 'Stop Proxy');
    await confirm(page, 'Stop Proxy');
  }, { wait: proxyIs('exited') });
  await step('proxy: back to traefik', async (side) => {
    const page = await open(side, '/server/{U}/proxy');
    await clickTop(page, 'Switch proxy');
    await confirm(page, 'Switch Proxy');
    await sleep(2500);
    await open(side, '/server/{U}/proxy');
    await page.locator('button:visible').filter({ hasText: /^\s*Traefik/ }).first().click();
  }, { wait: proxyIs('running'), settle: 8000 });
  await step('proxy: refresh status', async (side) => {
    const page = await open(side, '/server/{U}/proxy');
    await page.locator('.split-action-caret:visible').first().click();
    await clickTop(page, 'Refresh Proxy Status');
    await sleep(4000);
  });

  // Destinations.
  vm('ref', 'docker', 'network', 'create', 'diff-scan');
  vm('rs', 'docker', 'network', 'create', 'diff-scan');
  await step('destinations: scan', async (side) => {
    const page = await open(side, '/server/{U}/destinations');
    await clickTop(page, 'Scan networks');
    await sleep(4000);
  });
  await step('destinations: add scanned', async (side, page) => {
    await page.locator('div.flex.items-center.justify-between').filter({ hasText: 'diff-scan' }).getByRole('button', { name: 'Add destination' }).click();
    await sleep(3000);
  });
  await step('destinations: create', async (side) => {
    const page = await open(side, '/server/{U}/destinations');
    await clickTop(page, '+ Add');
    await sleep(700);
    await field(page, 'name').fill('diff created');
    await field(page, 'network').fill('diff-created-net');
    await clickTop(page, 'Create destination');
    await sleep(4000);
    server[side].dest = sql(`select uuid from standalone_dockers where server_id=${server[side].id} and network='diff-created-net'`);
    server[side].dest2 = sql(`select uuid from standalone_dockers where server_id=${server[side].id} and network='diff-scan'`);
  });
  await step('destinations: delete', async (side) => {
    const page = await open(side, '/destination/{D}/danger');
    await clickTop(page, /^\s*Delete destination\s*$/i);
    await confirm(page, 'Permanently Delete', { typed: 'diff created' });
    await sleep(3000);
  });
  await step('destinations: delete scanned', async (side) => {
    const page = pages[side];
    await page.goto(`${BASE[side]}/destination/${server[side].dest2}/danger`);
    await sleep(2000);
    const name = sql(`select name from standalone_dockers where uuid='${server[side].dest2}'`);
    await clickTop(page, /^\s*Delete destination\s*$/i);
    await confirm(page, 'Permanently Delete', { typed: name });
    await sleep(3000);
  }, { ignore: ['nets'] });
  vm('ref', 'docker', 'network', 'rm', 'diff-scan');
  vm('rs', 'docker', 'network', 'rm', 'diff-scan');

  // Unmanaged containers.
  vm('ref', 'docker', 'run', '-d', '--name', 'diff-unmanaged', 'busybox:1.36', 'sleep', '86400');
  vm('rs', 'docker', 'run', '-d', '--name', 'diff-unmanaged', 'busybox:1.36', 'sleep', '86400');
  const unmanaged = async (side, label) => {
    const page = await open(side, '/server/{U}/resources');
    await clickTop(page, 'Unmanaged');
    await sleep(3000);
    await page.locator('.data-table-row').filter({ hasText: 'diff-unmanaged' }).getByRole('button', { name: label, exact: true }).click();
    // `sleep` as PID 1 ignores SIGTERM: a restart takes the 10 seconds of docker's stop timeout.
    await sleep(label === 'Restart' ? 13000 : 4000);
  };
  const unmanagedIs = (state) => (side) => vm(side, 'docker', 'inspect', '--format', '{{.State.Status}}', 'diff-unmanaged').trim() === state;
  await step('unmanaged: restart', (side) => unmanaged(side, 'Restart'), { wait: unmanagedIs('running') });
  await step('unmanaged: stop', (side) => unmanaged(side, 'Stop'), { wait: unmanagedIs('exited') });
  await step('unmanaged: start', (side) => unmanaged(side, 'Start'), { wait: unmanagedIs('running') });
  vm('ref', 'docker', 'rm', '-f', 'diff-unmanaged');
  vm('rs', 'docker', 'rm', '-f', 'diff-unmanaged');

  // Images.
  vm('ref', 'docker', 'pull', '-q', 'hello-world:latest');
  vm('rs', 'docker', 'pull', '-q', 'hello-world:latest');
  await step('images: delete', async (side) => {
    const page = await open(side, '/server/{U}/images');
    await sleep(3000);
    const row = page.locator('.data-table-row, tr').filter({ hasText: 'hello-world' }).first();
    await row.getByRole('button', { name: 'Delete', exact: true }).click();
    await confirm(page, 'Delete Image', { typed: 'hello-world:latest' });
    await sleep(4000);
  });

  // Docker cleanup.
  vm('ref', 'docker', 'volume', 'create', 'diff-vol');
  vm('rs', 'docker', 'volume', 'create', 'diff-vol');
  await step('cleanup: run', async (side) => {
    const page = await open(side, '/server/{U}/docker-cleanup');
    await clickTop(page, 'Run cleanup');
    await confirm(page, 'Run Docker Cleanup');
    await sleep(3000);
  }, {
    wait: (side) => sql(`select count(*) from docker_cleanup_executions where server_id=${server[side].id} and finished_at is not null`) !== '0',
  });

  // Log drains.
  await step('logdrain: enable axiom', async (side) => {
    const page = await open(side, '/server/{U}/log-drains');
    await field(page, 'logDrainAxiomApiKey').fill('k1');
    await field(page, 'logDrainAxiomDatasetName').fill('d1');
    await listbox(page, 'isLogDrainAxiomEnabled', 'Enabled');
    await sleep(15000);
  }, { settle: 15000 });
  await step('logdrain: disable axiom', async (side) => {
    const page = await open(side, '/server/{U}/log-drains');
    await listbox(page, 'isLogDrainAxiomEnabled', 'Disabled');
    await sleep(8000);
  }, { settle: 8000 });

  // Sentinel.
  await step('sentinel: restart', async (side) => {
    const page = await open(side, '/server/{U}/sentinel');
    await clickTop(page, /^\s*(Restart|Sync)\s*$/);
    await sleep(4000);
  }, { settle: 15000, ignore: ['server.sentinel_waiting_since'] });
  await step('sentinel: regenerate token', async (side) => {
    const page = await open(side, '/server/{U}/sentinel');
    await clickTop(page, 'Regenerate token');
    await sleep(3000);
  }, { settle: 15000, ignore: ['server.sentinel_waiting_since'] });
  await step('sentinel: restore defaults', async (side) => {
    const page = await open(side, '/server/{U}/sentinel');
    await clickTop(page, 'Restore defaults');
    await confirm(page, 'Restore defaults');
    await sleep(3000);
  }, { settle: 15000, ignore: ['server.sentinel_waiting_since'] });

  // Registries: a local registry with a password on each VM.
  const htpasswd = 'diff:$2y$05$u2ZpI4f3J0N7X6v3b8v2Ue6mH3b3tJm1p0d8g9nq3l8yq1yq9Vt8a';
  for (const side of ['ref', 'rs']) {
    vm(side, 'mkdir', '-p', '/root/diff-registry');
    const real = vm(side, 'docker', 'run', '--rm', '--entrypoint', 'htpasswd', 'httpd:2.4-alpine', '-Bbn', 'diff', 'diff-pass').trim() || htpasswd;
    execFileSync('orb', ['-m', VM[side], '-u', 'root', 'tee', '/root/diff-registry/htpasswd'], { input: real + '\n', stdio: ['pipe', 'ignore', 'ignore'] });
    vm(side, 'docker', 'run', '-d', '--name', 'diff-registry', '-p', '127.0.0.1:5005:5000', '-v', '/root/diff-registry:/auth', '-e', 'REGISTRY_AUTH=htpasswd', '-e', 'REGISTRY_AUTH_HTPASSWD_REALM=diff', '-e', 'REGISTRY_AUTH_HTPASSWD_PATH=/auth/htpasswd', 'registry:2');
  }
  await sleep(3000);
  const login_ = async (side, pw) => {
    const page = await open(side, '/server/{U}/registries');
    await clickTop(page, 'New');
    await sleep(800);
    await listbox(page, 'provider', /Other|Custom/);
    await field(page, 'registry').fill('127.0.0.1:5005');
    await field(page, 'username').fill('diff');
    await field(page, 'password').fill(pw);
    await clickTop(page, 'Log in');
    await sleep(5000);
  };
  await step('registry: login wrong password', (side) => login_(side, 'wrong'));
  await step('registry: login', (side) => login_(side, 'diff-pass'));
  await step('registry: test', async (side) => {
    const page = await open(side, '/server/{U}/registries');
    await sleep(2000);
    await clickTop(page, 'Test');
    await sleep(4000);
  });
  await step('registry: logout', async (side) => {
    const page = await open(side, '/server/{U}/registries');
    await sleep(2000);
    await page.locator('div').filter({ hasText: '127.0.0.1:5005' }).locator('button:visible').filter({ hasText: /^\s*Delete\s*$/ }).last().click();
    await confirm(page, 'Continue');
    await sleep(4000);
  });
  vm('ref', 'docker', 'rm', '-f', 'diff-registry');
  vm('rs', 'docker', 'rm', '-f', 'diff-registry');

  // CA certificate.
  const caPath = '/data/coolify/ssl/coolify-ca.crt';
  await step('ca: save', async (side) => {
    const page = await open(side, '/server/{U}/ca-certificate');
    await clickTop(page, /^\s*Show certificate\s*$/i).catch(() => {});
    await sleep(800);
    const area = field(page, 'certificateContent');
    const current = await area.inputValue();
    await area.fill(current.trimEnd() + '\n');
    await clickTop(page, 'Save certificate');
    await confirm(page, 'Continue', { typed: caPath, step3: 'Save Certificate' }).catch(async () => confirm(page, 'Confirm', { typed: caPath, step3: 'Save Certificate' }));
    await sleep(5000);
  }, { ignore: ['ca'] });
  await step('ca: save invalid', async (side) => {
    const page = await open(side, '/server/{U}/ca-certificate');
    await clickTop(page, /^\s*Show certificate\s*$/i).catch(() => {});
    await sleep(800);
    await field(page, 'certificateContent').fill('not a certificate');
    await clickTop(page, 'Save certificate');
    await confirm(page, 'Continue', { typed: caPath, step3: 'Save Certificate' }).catch(async () => confirm(page, 'Confirm', { typed: caPath, step3: 'Save Certificate' }));
    await sleep(4000);
  }, { ignore: ['ca'] });
  await step('ca: regenerate', async (side) => {
    const page = await open(side, '/server/{U}/ca-certificate');
    await clickTop(page, 'Regenerate');
    await confirm(page, 'Continue', { typed: caPath, step3: 'Regenerate Certificate' }).catch(async () => confirm(page, 'Confirm', { typed: caPath, step3: 'Regenerate Certificate' }));
    await sleep(5000);
  }, { ignore: ['ca'] });

  // TLS certificates: a fixture store with two certificates.
  const acme = JSON.stringify({ letsencrypt: { Account: { Email: 'a@example.test', Registration: {}, PrivateKey: 'x', KeyType: '4096' }, Certificates: [
    { domain: { main: 'one.example.test', sans: ['www.one.example.test'] }, certificate: '', key: 'k', Store: 'default' },
    { domain: { main: 'two.example.test' }, certificate: '', key: 'k', Store: 'default' },
  ] } }, null, 2) + '\n';
  for (const side of ['ref', 'rs']) {
    execFileSync('orb', ['-m', VM[side], '-u', 'root', 'tee', '/data/coolify/proxy/acme.json'], { input: acme, stdio: ['pipe', 'ignore', 'ignore'] });
    vm(side, 'sh', '-c', 'rm -f /data/coolify/proxy/acme.json.backup-*; chmod 600 /data/coolify/proxy/acme.json');
  }
  const acmeState = (side) => normalize(vm(side, 'cat', '/data/coolify/proxy/acme.json'), server[side].names);
  await step('tls: delete certificate', async (side) => {
    const page = await open(side, '/server/{U}/proxy/certificates');
    await sleep(2500);
    await page.locator('tr').filter({ hasText: 'one.example.test' }).getByRole('button', { name: 'Delete', exact: true }).click();
    await confirm(page, 'Delete Certificate', { typed: 'one.example.test' });
    await sleep(4000);
  });
  verdict('tls: acme.json after delete', acmeState('ref') === acmeState('rs'), `ref ${acmeState('ref')}\n     rs  ${acmeState('rs')}`);
  await step('tls: restore backup', async (side) => {
    const page = await open(side, '/server/{U}/proxy/certificates');
    await sleep(2500);
    await page.locator('tr').filter({ hasText: 'acme.json.backup-' }).first().getByRole('button', { name: 'Restore', exact: true }).click();
    await sleep(500);
    await page.locator('[role=dialog]:visible, .fixed:visible').getByRole('checkbox').first().uncheck({ timeout: 3000 }).catch(() => {});
    await clickTop(page, 'Continue').catch(() => {});
    await confirm(page, 'Restore Backup');
    await sleep(4000);
  });
  verdict('tls: acme.json after restore', acmeState('ref') === acmeState('rs') && acmeState('rs') === acme, `ref ${acmeState('ref')}\n     rs  ${acmeState('rs')}`);
  await step('tls: delete backup', async (side) => {
    const page = await open(side, '/server/{U}/proxy/certificates');
    await sleep(2500);
    await page.locator('tr').filter({ hasText: 'acme.json.backup-' }).first().getByRole('button', { name: 'Delete', exact: true }).click();
    await confirm(page, 'Delete Backup');
    await sleep(4000);
  });
  for (const side of ['ref', 'rs']) {
    vm(side, 'sh', '-c', 'rm -f /data/coolify/proxy/acme.json /data/coolify/proxy/acme.json.backup-*');
    const m = json(`select proxy::text from servers where id=${server[side].id}`);
    delete m.certificates_restart_required;
    sql(`update servers set proxy='${JSON.stringify(m).replace(/'/g, "''")}' where id=${server[side].id}`);
  }

  // Private key: a second key, authorized on both VMs. The switch through ssh-via-orb does not use keys on the
  // coolify-rs side, so what it proves there is the rows and the toast.
  const keyDir = fs.mkdtempSync(`${os.tmpdir()}/diff-key-`);
  execFileSync('ssh-keygen', ['-q', '-t', 'ed25519', '-N', '', '-f', `${keyDir}/k`]);
  const created = await api('ref', 'POST', '/api/v1/security/keys', { name: 'diff-ui-key', private_key: fs.readFileSync(`${keyDir}/k`, 'utf8') });
  const newKey = JSON.parse(created.body).uuid;
  const pub = fs.readFileSync(`${keyDir}/k.pub`, 'utf8');
  fs.rmSync(keyDir, { recursive: true, force: true });
  for (const side of ['ref', 'rs']) {
    const current = vm(side, 'cat', '/root/.ssh/authorized_keys');
    execFileSync('orb', ['-m', VM[side], '-u', 'root', 'tee', '/root/.ssh/authorized_keys'], { input: current.trimEnd() + '\n' + pub, stdio: ['pipe', 'ignore', 'ignore'] });
  }
  const useKey = async (side, name) => {
    const page = await open(side, '/server/{U}/private-key');
    await sleep(1500);
    await page.locator('div.flex.flex-col.gap-4').filter({ hasText: name }).getByRole('button', { name: 'Use this key', exact: true }).click();
    await sleep(5000);
  };
  const keyName = () => sql(`select name from private_keys where id=(select private_key_id from servers where uuid='${server.ref.uuid}')`);
  await step('key: switch', (side) => useKey(side, 'diff-ui-key'));
  verdict('key: active key after switch', sql(`select count(distinct private_key_id) from servers where uuid in ('${server.ref.uuid}','${server.rs.uuid}')`) === '1' && keyName() === 'diff-ui-key', keyName());
  await step('key: switch back', (side) => useKey(side, 'orbstack e2e'));
  sql(`delete from private_keys where uuid='${newKey}'`);
  for (const side of ['ref', 'rs']) {
    execFileSync('orb', ['-m', VM[side], '-u', 'root', 'tee', '/root/.ssh/authorized_keys'], { input: vm(side, 'cat', '/root/.ssh/authorized_keys').split('\n').filter((l) => l && l !== pub.trim()).join('\n') + '\n', stdio: ['pipe', 'ignore', 'ignore'] });
  }

  // Metrics.
  await step('metrics: enable', async (side) => {
    const page = await open(side, '/server/{U}/metrics');
    await clickTop(page, 'Enable metrics');
    await sleep(8000);
  }, { settle: 5000 });
  await step('metrics: disable', async (side) => {
    const page = await open(side, '/server/{U}/metrics');
    await clickTop(page, 'Disable metrics');
    await sleep(5000);
  }, { settle: 12000 });

  await step('key: check connection', async (side) => {
    const page = await open(side, '/server/{U}/private-key');
    await clickTop(page, /^\s*Check connection\s*$/i);
    await sleep(4000);
  });
}
