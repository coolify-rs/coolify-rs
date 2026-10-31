// `api.auth.unauthenticated` and `api.auth.ability_denied` on both sides. The requests carry their own
// User-Agent; only rows with that exact value are read and then deleted, by id. The ability check uses
// a DELETE of a project that does not exist, so nothing changes even if it were let through.
//   node compat/diff/auth_audit_diff.mjs 18086 <read-only token name in compat/reference/secrets/tokens.json>
import { psql, tokens, diff } from '../ui/flow.mjs';

const [port, tokenName] = process.argv.slice(2);
const agent = `batd-auth-audit-${process.pid}`;
const bases = { ref: 'http://127.0.0.1:18000', rs: `http://127.0.0.1:${port}` };
const results = {};
for (const [side, base] of Object.entries(bases)) {
  const r = {};
  const call = async (path, init = {}) => {
    const res = await fetch(base + path, { ...init, headers: { Accept: 'application/json', 'User-Agent': agent, ...init.headers } });
    return [res.status, await res.json().catch(() => null)];
  };
  // Twice: the second one falls in the deduplication window.
  r.bad1 = await call('/api/v1/teams', { headers: { Authorization: 'Bearer nope' } });
  r.bad2 = await call('/api/v1/teams', { headers: { Authorization: 'Bearer nope' } });
  r.none = await call('/api/v1/teams');
  r.denied = await call('/api/v1/projects/batd-does-not-exist', { method: 'DELETE', headers: { Authorization: `Bearer ${tokens[tokenName]}` } });
  await new Promise((f) => setTimeout(f, 800));
  const rows = JSON.parse(psql(`select coalesce(json_agg(x),'[]') from (select id,event,team_id,level,actor_type,actor_id,actor_token_name,resource_type,description,metadata from audit_events where user_agent='${agent}' order by id) x`));
  r.rows = rows.map(({ id, metadata, ...a }) => ({ ...a, metadata: { ...metadata, token_id: metadata?.token_id ? 'x' : undefined } }));
  console.log(side, rows.map((x) => x.event));
  results[side] = r;
  if (rows.length) psql(`delete from audit_events where id in (${rows.map((x) => x.id).join(',')})`);
}
for (const k of Object.keys(results.ref)) diff(k, results.ref[k], results.rs[k]);
