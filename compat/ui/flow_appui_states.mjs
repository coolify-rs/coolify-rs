// Application and service page states, the same steps on both sides, as owner and as member:
// running task executions and Execute now, the degraded and unreachable badges, deployment lists with
// ?pull_request_id, the deployment log, the rollback skeleton, Compose load success against a git repository
// on the test VM, Load pull requests against a local GitHub fake, and changing a service backup schedule.
//   python3 compat/ui/screens2_fixtures.py create <state.json>
//   (a bare repository served at http://192.168.139.220:19419/nobody/nothing on the VM)
//   node compat/ui/flow_appui_states.mjs <state.json> 18088
// Nothing reaches github.com: the application's source is a github_apps row of this script.
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import http from 'node:http';
import { flow, psql, tokens } from './flow.mjs';

const [stateFile, port] = process.argv.slice(2);
const S = JSON.parse(fs.readFileSync(stateFile, 'utf8'));
const base = `/project/${S.project}/environment/${S.env}`;
const REF = 'http://127.0.0.1:18000';
const clean = (s) => s.replace(/\s+/g, ' ').trim();
const vm = (c) => execFileSync('orb', ['-m', 'coolify-rs-e2e', '-u', 'root', 'bash', '-c', c], { encoding: 'utf8' }).trim();
async function api(method, path, body) {
  const r = await fetch(REF + path, {
    method,
    headers: { Authorization: `Bearer ${tokens.acme_owner_root}`, Accept: 'application/json', 'Content-Type': 'application/json' },
    body: body ? JSON.stringify(body) : undefined,
  });
  return { status: r.status, body: await r.json().catch(() => null) };
}

// A GitHub API of this script.
const pulls = [
  { number: 7, title: 'Second change', html_url: 'http://fake/7', head: { ref: 'b', sha: 'bbbb' }, user: { login: 'dev' } },
  { number: 3, title: 'First change', html_url: 'http://fake/3', head: { ref: 'a', sha: 'aaaa' }, user: { login: 'dev' } },
];
const fakeSeen = [];
const fake = http.createServer((req, res) => {
  fakeSeen.push(`${req.method} ${req.url}`);
  res.setHeader('Content-Type', 'application/json');
  res.setHeader('X-RateLimit-Remaining', '4321');
  res.end(JSON.stringify(req.url.includes('/pulls') ? pulls : {}));
});
await new Promise((ok) => fake.listen(18199, '0.0.0.0', ok));

const appId = (u) => psql(`select id from applications where uuid='${u}'`);
const image = S.apps.image;
const imageId = appId(image);
const made = { deployments: [], tasks: [], github: null };

// Setup. Only rows of the fixture project, tracked by id.
const logs = JSON.stringify([
  { command: null, output: 'Starting deployment', type: 'stdout', timestamp: '2026-01-01T00:00:00.000000Z', hidden: false, batch: 1, order: 1 },
  { command: 'docker pull nginx', output: 'Pulled', type: 'stdout', timestamp: '2026-01-01T00:00:01.000000Z', hidden: true, batch: 1, order: 2 },
  { command: null, output: 'Something failed', type: 'stderr', timestamp: '2026-01-01T00:00:02.000000Z', hidden: false, batch: 1, order: 3 },
]).replaceAll("'", "''");
for (const [pr, uuid] of [[0, 'screens2dep0000000000000'], [1, 'screens2dep1111111111111']]) {
  made.deployments.push(psql(`insert into application_deployment_queues (application_id,deployment_uuid,status,server_id,application_name,server_name,created_at,updated_at,pull_request_id,force_rebuild,commit,restart_only,only_this_server,rollback,logs) values ('${imageId}','${uuid}','failed',57,'s2-image','orbstack-e2e','2026-01-01 00:00:00','2026-01-01 00:01:00',${pr},false,'HEAD',false,false,false,'${logs}') returning id`).split('\n')[0]);
}
made.github = psql(`insert into github_apps (uuid,name,api_url,html_url,custom_user,custom_port,is_public,team_id,created_at,updated_at) values ('screens2githubfake000000','s2-fake','http://host.docker.internal:18199','http://192.168.139.220:19419','git',22,true,4,now(),now()) returning id`).split('\n')[0];
for (const u of [S.apps.git, S.apps.compose]) {
  psql(`update applications set source_type='App\\Models\\GithubApp', source_id=${made.github}, git_repository='nobody/nothing', git_branch='main', docker_compose_location='/docker-compose.yaml', base_directory='/' where uuid='${u}'`);
}
psql(`update application_settings set is_preview_deployments_enabled=true where application_id=${appId(S.apps.git)}`);
const task = await api('POST', `/api/v1/applications/${image}/scheduled-tasks`, { name: 's2-task', command: 'echo hello-from-task', frequency: 'daily' });
const taskId = psql(`select id from scheduled_tasks where uuid='${task.body.uuid}'`);
// A real deployment, so the task has a container and the status jobs leave the status alone.
const deployed = await api('POST', `/api/v1/deploy?uuid=${image}&force=false`);
console.log('deploy', deployed.status);
for (let i = 0; i < 120; i++) {
  if (psql(`select status from applications where id=${imageId}`).startsWith('running')) break;
  await new Promise((ok) => setTimeout(ok, 2000));
}
const running = psql(`insert into scheduled_task_executions (uuid,scheduled_task_id,status,message,started_at,retry_count,created_at,updated_at) values ('screens2execrunning00000',${taskId},'running',null,now(),0,now(),now()) returning id`).split('\n')[0];
// A storage backup schedule on the service.
const storage = psql(`select v.uuid from local_persistent_volumes v join service_databases d on d.id=v.resource_id and v.resource_type like '%ServiceDatabase' join services s on s.id=d.service_id where s.uuid='${S.services[0]}' limit 1`);
const sched = await api('PUT', `/api/v1/services/${S.services[0]}/storages/${storage}/backups`, { frequency: 'daily' });
const savedRow = () => psql(`select frequency, enabled, retention_amount_locally, stop_during_backup from scheduled_volume_backups b join local_persistent_volumes v on v.id=b.backupable_id where v.uuid='${storage}' and b.backupable_type like '%LocalPersistentVolume'`);

const main = (h) => h.page.locator('main').first();
const text = async (h) => clean(await main(h).innerText()).replace(/\d+ ?(seconds?|minutes?|hours?|days?) ago|\d{4}-\d\d-\d\d[ T][\d:.]+Z?( UTC)?|\b\d+(s|m|h)\b/g, 'TIME');

try {
  for (const user of ['alice', 'bob']) {
    await flow(port, user, async (h) => {
      const r = {};
      const tag = user === 'bob' ? 'mem-' : '';
      const app = `${base}/application/${image}`;
      if (user === 'alice') psql(`update github_apps set api_url='${h.label === 'ref' ? 'http://host.docker.internal:18199' : 'http://127.0.0.1:18199'}' where id=${made.github}`);

      // Task with a running execution. Tasks show their rows in a modal.
      await h.go(`${app}/scheduled-tasks`);
      await h.page.waitForTimeout(1000);
      r.tasks = await text(h);
      await h.shot(`${tag}st-tasks`);
      await h.page.locator('main').getByText('s2-task').first().click();
      await h.page.waitForTimeout(1200);
      await h.shot(`${tag}st-task`, false);
      // A digit is the team's deployment counter, which other work moves.
      r.taskButtons = (await h.page.locator('.fixed button:visible').allInnerTexts()).map(clean).filter((x) => x && !/^\d+$/.test(x));
      const executions = h.page.locator('.fixed button:visible', { hasText: /^\s*Executions\s*$/ }).first();
      if (await executions.count()) {
        await executions.click();
        await h.page.waitForTimeout(800);
        r.executions = clean(await h.page.locator('.fixed:visible').last().innerText()).replace(/\d+ ?(seconds?|minutes?|hours?) ago|\d{4}-\d\d-\d\d[ T][\d:.]+Z?( UTC)?/g, 'TIME');
        await h.shot(`${tag}st-execs`, false);
      }
      if (user === 'alice') {
        const general = h.page.locator('.fixed button:visible', { hasText: /^\s*General\s*$/ }).first();
        if (await general.count()) await general.click();
        await h.page.waitForTimeout(400);
        const max = psql(`select coalesce(max(id),0) from scheduled_task_executions where scheduled_task_id=${taskId}`);
        await h.page.locator('.fixed button:visible', { hasText: 'Execute now' }).click({ timeout: 6000 }).catch(async (e) => {
          await h.shot('st-execnow-debug', false);
          console.log('no Execute now on', h.label, psql(`select status from applications where id=${imageId}`));
          throw e;
        });
        r.executeToast = await h.toasts(1500);
        let row = '';
        for (let i = 0; i < 40; i++) {
          row = psql(`select status, trim(message), retry_count from scheduled_task_executions where scheduled_task_id=${taskId} and id>${max} order by id limit 1`);
          if (row && !row.startsWith('running')) break;
          await h.page.waitForTimeout(1000);
        }
        r.executeRow = row;
        psql(`delete from scheduled_task_executions where scheduled_task_id=${taskId} and id>${max}`);
      }

      // Status badges. The reference's heading starts a status check on opening, so that request is held back.
      const statusNow = psql(`select status from applications where id=${imageId}`);
      const other = S.apps.dockerfile;
      const otherId = appId(other);
      await h.page.route('**/livewire/update', (route) => route.abort());
      const open = async (name, status) => {
        for (let attempt = 0; attempt < 4; attempt++) {
          psql(`update applications set status='${status}' where id=${otherId}`);
          await h.page.goto(h.base + `${base}/application/${other}`);
          await h.page.waitForTimeout(500);
          // The configuration checker and the deployment counter load through the held request: only the status counts.
          if (psql(`select status from applications where id=${otherId}`) !== status) continue;
          r[`badge.${name}`] = clean(await h.page.locator('body').innerText()).match(/s2-dockerfile (\w+)/)?.[1];
          r[`badge.${name}.degradedTitle`] = await h.page.locator('[title="Application is in degraded state across multiple servers."]:visible').count();
          await h.shot(`${tag}st-badge-${name}`, false);
          return;
        }
        r[`badge.${name}`] = 'status kept changing';
      };
      await open('running', 'running:healthy');
      await open('degraded', 'degraded:unhealthy');
      // With an additional server as well: `ServerStatusBadge`.
      const dest = psql(`select id from standalone_dockers where server_id=57 order by id limit 1`);
      psql(`insert into additional_destinations (application_id,server_id,standalone_docker_id,status,created_at,updated_at) values (${otherId},57,${dest},'running:healthy',now(),now())`);
      await open('degraded-additional', 'degraded:unhealthy');
      await h.page.unroute('**/livewire/update');
      psql(`delete from additional_destinations where application_id=${otherId} and server_id=57`);
      psql(`update applications set status='exited' where id=${otherId}`);
      void statusNow;

      // Deployments with and without ?pull_request_id.
      for (const [name, q] of [['all', ''], ['pr0', '?pull_request_id=0'], ['pr1', '?pull_request_id=1'], ['pr9', '?pull_request_id=9'], ['bad', '?pull_request_id=abc']]) {
        await h.go(`${app}/deployment${q}`);
        await h.page.waitForTimeout(900);
        r[`deployments.${name}`] = await text(h);
        r[`deployments.${name}.url`] = new URL(h.page.url()).search;
        await h.shot(`${tag}st-dep-${name}`, false);
      }
      await h.go(`${app}/deployment/screens2dep1111111111111`);
      await h.page.waitForTimeout(1200);
      r.deploymentLog = await text(h);
      await h.shot(`${tag}st-log`);

      // Rollback: holds what loads the images, so the skeleton stays on screen.
      const rollbackImages = /livewire\/update|\/ui\/.*rollback$/;
      await h.page.route(rollbackImages, async (route) => {
        await new Promise((ok) => setTimeout(ok, 2500));
        await route.continue().catch(() => {});
      });
      await h.page.goto(h.base + `${app}/rollback`);
      await h.page.waitForTimeout(1200);
      await h.shot(`${tag}st-rollback-skeleton`, false);
      await h.page.unroute(rollbackImages);
      for (let i = 0; i < 30; i++) {
        const t = await text(h);
        if (t.includes('Images to keep') && !t.includes('Loading available images')) break;
        await h.page.waitForTimeout(1000);
      }
      r.rollback = await text(h);
      await h.shot(`${tag}st-rollback`, false);

      // Compose load success, from the repository on the VM. The page loads it on opening.
      psql(`update applications set docker_compose_raw=null, docker_compose=null where uuid='${S.apps.compose}'`);
      await h.go(`${base}/application/${S.apps.compose}`);
      await h.page.waitForTimeout(500);
      const loadCompose = h.page.locator('button:visible', { hasText: /^\s*Load compose\s*$/i }).first();
      r.composeButton = await loadCompose.count();
      if (user === 'alice') await loadCompose.click();
      for (let i = 0; i < 25; i++) {
        if (psql(`select docker_compose_raw is not null from applications where uuid='${S.apps.compose}'`) === 't') break;
        await h.page.waitForTimeout(1000);
      }
      await h.page.waitForTimeout(1500);
      r.composeRow = psql(`select md5(docker_compose_raw) from applications where uuid='${S.apps.compose}'`);
      r.composeToast = await h.toasts(500);
      r.composeReload = await h.page.locator('button:visible', { hasText: 'Reload compose' }).isEnabled().catch(() => 'absent');
      await h.shot(`${tag}st-compose`);

      // Load pull requests.
      await h.go(`${base}/application/${S.apps.git}/preview-deployments`);
      await h.page.waitForTimeout(1000);
      r.previews = await text(h);
      await h.shot(`${tag}st-prev`);
      const load = h.page.locator('button:visible', { hasText: 'Load pull requests' });
      r.loadVisible = await load.count();
      if (user === 'alice' && r.loadVisible) {
        fakeSeen.length = 0;
        await load.click();
        await h.page.waitForTimeout(2500);
        r.pullsToast = await h.toasts(300);
        r.pulls = clean(await h.page.locator('.fixed:visible', { hasText: 'Load open pull requests' }).last().innerText());
        r.pullsRequest = fakeSeen.filter((x) => x.includes('/pulls')).map((x) => x.split(' ')[0]);
        await h.shot(`st-prev-loaded`);
      }

      // Service backups: list and a changed schedule.
      await h.go(`${base}/service/${S.services[0]}/storage-backups`);
      await h.page.waitForTimeout(1200);
      r.backups = await text(h);
      await h.shot(`${tag}st-backups`);
      const edit = h.page.locator('main button.icon-button:visible:has(svg)').last();
      r.editDisabled = await edit.isDisabled();
      if (!r.editDisabled) {
        await edit.click();
        await h.page.waitForTimeout(1200);
        await h.shot(`${tag}st-backup-edit`, false);
        const fq = h.field('Frequency');
        r.frequencyEnabled = await fq.isEditable().catch(() => 'absent');
        if (user === 'alice' && r.frequencyEnabled === true) {
          await fq.fill('weekly');
          await h.page.waitForTimeout(400);
          await h.shot('st-backup-dirty', false);
          r.unsaved = clean(await h.page.locator('.fixed:visible').last().innerText()).includes('Unsaved') ;
          await h.page.locator('.fixed button:visible', { hasText: /^\s*Save/ }).first().click();
          r.saveToast = await h.toasts(1500);
          r.savedRow = savedRow();
          psql(`update scheduled_volume_backups set frequency='daily' where uuid='${sched.body?.uuid ?? ''}'`);
        }
      }
      console.log(h.label, user, JSON.stringify({ execute: r.executeRow, compose: r.composeRow, composeToast: r.composeToast, pulls: r.pullsToast, saved: r.savedRow, save: r.saveToast, unsaved: r.unsaved, badge: r['badge.degraded-additional.degradedTitle'], editDisabled: r.editDisabled }));
      return r;
    });
  }
} finally {
  await api('POST', `/api/v1/applications/${image}/stop`);
  for (let i = 0; i < 30; i++) {
    if (!vm(`docker ps -q --filter name=${image}`)) break;
    await new Promise((ok) => setTimeout(ok, 2000));
  }
  psql(`delete from scheduled_task_executions where scheduled_task_id=${taskId}`);
  await api('DELETE', `/api/v1/applications/${image}/scheduled-tasks/${task.body.uuid}`);
  void running;
  for (const u of [S.apps.git, S.apps.compose]) psql(`update applications set source_id=null, source_type=null where uuid='${u}'`);
  psql(`delete from github_apps where id=${made.github}`);
  for (const d of made.deployments) psql(`delete from application_deployment_queues where id=${d}`);
  fake.close();
}
