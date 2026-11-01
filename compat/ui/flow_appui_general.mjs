// Application General: the sections that had no backend. Same steps on both UIs, on the fixtures of
// compat/diff/appui_fixture.py; every field is put back between the sides.
//   node compat/ui/flow_appui_general.mjs <port>
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { flow, psql, saveRow, restoreRow } from './flow.mjs';

const [port] = process.argv.slice(2);
const F = JSON.parse(fs.readFileSync(path.join(os.tmpdir(), 'coolify-rs-appui-fixture.json'), 'utf8'));
const base = `/project/${F.project}/environment/${F.environment}`;
const cols = (uuid) =>
  JSON.parse(
    psql(`select row_to_json(r) from (select a.custom_docker_run_options, a.pre_deployment_command, a.pre_deployment_command_container, a.post_deployment_command, a.post_deployment_command_container, a.docker_compose_custom_build_command, a.docker_compose_custom_start_command, a.is_http_basic_auth_enabled, a.http_basic_auth_username, a.http_basic_auth_password is not null as has_password, a.http_basic_auth_hash_algorithm, a.http_basic_auth_bcrypt_cost, s.is_preserve_repository_enabled, s.is_container_label_escape_enabled from applications a join application_settings s on s.application_id = a.id where a.uuid = '${uuid}') r`),
  );
const saved = () => [F.image, F.compose].flatMap((u) => [saveRow('applications', `uuid = '${u}'`), saveRow('application_settings', `application_id = (select id from applications where uuid = '${u}')`)]);
let snapshot;

await flow(
  port,
  'alice',
  async (h) => {
    const rec = {};
    h.page.setDefaultTimeout(10000);
    const save = async () => {
      await h.page.locator('body').click({ position: { x: 900, y: 5 } });
      await h.page.waitForTimeout(300);
      await h.page.keyboard.press('Enter');
      return h.toasts(2000);
    };

    // A Docker image application.
    await h.go(`${base}/application/${F.image}`);
    await h.shot('gen-image');
    await h.field('Custom Docker options').fill('--cap-add SYS_ADMIN');
    await h.field('Pre-deployment').fill('echo pre');
    await h.field('Post-deployment').fill('echo post');
    await h.field('Name').focus();
    await h.page.keyboard.press('Enter');
    rec.imageSaveToast = await h.toasts(2000);
    rec.imageCols = cols(F.image);
    await h.field('Custom Docker options').fill('bad;`x`');
    await h.field('Name').focus();
    await h.page.keyboard.press('Enter');
    await h.page.waitForTimeout(1500);
    rec.imageBadOptions = await h.page.locator('text=custom Docker run options contain invalid').count().then((n) => n > 0);

    // HTTP Basic Authentication.
    await h.go(`${base}/application/${F.image}`);
    await h.listbox('Method', 'HTTP Basic Authentication');
    rec.authToast = await h.toasts(2000);
    await h.page.waitForTimeout(800);
    await h.shot('gen-auth');
    await h.field('Username').fill('someone');
    await h.page.locator('input[type=password]:visible').first().fill('hunter22');
    await h.field('Name').focus();
    await h.page.keyboard.press('Enter');
    rec.authSaveToast = await h.toasts(2000);
    await h.listbox('Cost factor', '12');
    rec.costToast = await h.toasts(8000); // bcrypt in a debug build is slow
    rec.authCols = cols(F.image);
    rec.authLabels = /basicauth/i.test(Buffer.from(psql(`select custom_labels from applications where uuid = '${F.image}'`), 'base64').toString());

    // A Compose application.
    await h.go(`${base}/application/${F.compose}`);
    await h.page.waitForTimeout(1200);
    await h.shot('gen-compose');
    rec.composeHeading = await h.page.locator('h3:visible', { hasText: 'Docker Compose' }).count();
    await h.page.locator('button:visible', { hasText: /Show deployable compose/i }).click();
    await h.page.waitForTimeout(800);
    await h.shot('gen-compose-deployable');
    await h.field('Custom build command').fill('docker compose build');
    await h.field('Custom start command').fill('docker compose up -d');
    await h.field('Name').focus();
    await h.page.keyboard.press('Enter');
    rec.composeSaveToast = await h.toasts(5000);
    rec.composeCols = cols(F.compose);
    rec.composeDomains = psql(`select docker_compose_domains from applications where uuid = '${F.compose}'`);
    await h.page.waitForTimeout(1200);
    rec.buildPreview = await h.field('Final build command').inputValue();
    rec.startPreview = await h.field('Final start command').inputValue();
    await h.shot('gen-compose-custom');
    await h.page.locator('label:visible', { hasText: 'Preserve repository' }).first().click();
    rec.preserveToast = await h.toasts(2000);
    await h.page.locator('label:visible', { hasText: 'Escape special characters' }).first().click();
    rec.escapeToast = await h.toasts(2000);
    rec.composeCols2 = cols(F.compose);
    return rec;
  },
  {
    before: () => {
      snapshot = saved();
    },
    after: () => {
      for (const s of snapshot) restoreRow(s);
    },
  },
);
