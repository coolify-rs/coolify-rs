// Test users for the team and profile flows. They log in with the shared test password (their hash
// is copied, never printed). Never use alice, bob or carol for flows that change a user.
//   node compat/ui/flow_test_users.mjs create batb1 batb2   (or: delete batb1 batb2)
import { execFileSync } from 'node:child_process';
import { psql } from './flow.mjs';

export function tinker(code) {
  return execFileSync('docker', ['exec', 'coolify-rs-reference-coolify-1', 'php', '/var/www/html/artisan', 'tinker', '--execute', code], { encoding: 'utf8' }).trim();
}

/** Creates `<name>@example.test` through `User::create`, so its personal team has every row. */
export function createUser(name) {
  deleteUser(name);
  tinker(`$h = App\\Models\\User::where("email","alice@example.test")->value("password");
    $u = App\\Models\\User::create(["name"=>"${name}","email"=>"${name}@example.test","password"=>$h]);
    App\\Models\\Team::whereIn("id",$u->teams()->pluck("teams.id"))->update(["show_boarding"=>false]);
    Illuminate\\Support\\Facades\\Cache::forget("user:".$u->id.":team:".$u->teams()->first()->id);`);
  return Number(psql(`select id from users where email='${name}@example.test'`));
}

/** Deletes the user, the teams only they belong to and their audit rows, by exact id. */
export function deleteUser(name) {
  const id = psql(`select id from users where email='${name}@example.test'`);
  if (!id) return;
  const solo = psql(`select string_agg(tu.team_id::text, ',') from team_user tu where tu.user_id=${id} and tu.team_id<>0 and (select count(*) from team_user o where o.team_id=tu.team_id)=1`);
  tinker(`App\\Models\\User::whereKey(${id})->first()?->passkeys()->delete(); DB::table("team_user")->where("user_id",${id})->delete(); ${
    solo ? `App\\Models\\Team::whereIn("id",[${solo}])->get()->each->delete();` : ''
  } DB::table("users")->where("id",${id})->delete();`);
  psql(`delete from audit_events where actor_id=${id}`);
  if (solo) psql(`delete from audit_events where team_id in (${solo})`);
}

if (process.argv[1]?.endsWith('flow_test_users.mjs')) {
  const [cmd, ...names] = process.argv.slice(2);
  for (const n of names) {
    if (cmd === 'create') console.log(n, createUser(n));
    else deleteUser(n);
  }
}
