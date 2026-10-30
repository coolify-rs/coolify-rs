# Porting brief

Rules for anyone porting an upstream Coolify area to coolify-rs. The goal is behaviour identical to upstream:
same API responses (key order, messages, status codes), same database rows, same commands on servers, same UI.

## Setup in a git worktree

Git-ignored things are missing in a new worktree. Link them once (M = the main checkout,
`/Users/azeemhassni/Code/coolify-rs`):

    ln -s M/.upstream .upstream
    ln -s M/compat/reference/secrets compat/reference/secrets
    ln -s M/compat/ui/node_modules compat/ui/node_modules
    (cd frontend && npm install --silent && npm run build)

- Upstream source: `.upstream/snapshot` (Coolify main @ b5823c7). The PHP is the specification; read it, do not guess.
- Spec notes from earlier work: `compat/notes/`. They can be wrong; the PHP wins. Add a note there only for a large area.
- Remaining work per area: `scripts/parity.py --missing`.

## Environment

- Reference Coolify: http://127.0.0.1:18000. It shares one Postgres with coolify-rs (container
  `coolify-rs-reference-postgres-1`, `psql -U coolify -d coolify`). Run PHP with
  `docker exec coolify-rs-reference-coolify-1 php /var/www/html/artisan tinker --execute '...'`.
- coolify-rs: `scripts/dev-server.sh <your port>` builds and (re)starts your instance. Use only the port you were given.
  Do not touch other ports. A restart kills deployments your instance is running.
- Test server: uuid `hml20fsj5eles0xfiqidq8j5`, OrbStack VM `coolify-rs-e2e` (`orb -m coolify-rs-e2e -u root bash -c '...'`),
  project `j8xm2emlllhfnyebjzgyzp7z`, environment uuid `uj3st9yvtvzxh7rul3c3opcz` (production).
- API differential helpers: `compat/diff/databases_diff.py` (`call(side, method, path, body, tok="acme_owner_root")`,
  `sql()`, `record()`, `same()`). Set `COOLIFY_RS_BASE=http://127.0.0.1:<port>` to aim the `rs` side at your instance.
- UI: `cd compat/ui && node shots.mjs --rs=<port> [--full] name=/path[@rsPath]`, then `python3 pair.py name`, then read
  `compat/ui/shots/<name>-pair.png` (reference left). Login is alice@example.test, password in
  `compat/reference/secrets/ui-login.txt`; never print it. For interactions write a Playwright script next to it.
- Rust tests: `COOLIFY_RS_TEST_DATABASE_URL=postgres://coolify_rs:coolify_rs@127.0.0.1:25432/coolify_rs cargo test --workspace --all-features`.

## Definition of done

1. API: a differential script in `compat/diff/` compares every ported route with the reference: success and error
   paths, several tokens, resulting rows.
2. Jobs and server work: run for real on the test VM on both sides; compare commands, files, containers, log lines.
3. UI: every screen and state compared by screenshot, in dark mode, and the rows created through both UIs diffed.
   Same markup and classes as the Blade view (upstream CSS is vendored, `frontend/src/upstream/css/app.css`).
4. `cargo fmt --all`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, the tests, and
   `npx svelte-check --threshold error` pass before each commit.
5. `docs/FEATURE_INVENTORY.md` and `docs/KNOWN_LIMITATIONS.md` say what is ported and what is not.

Report only what you verified. Say plainly what failed, what you skipped and what you could not test.

## Rules

- A hook blocks any Bash command containing the text ".env" (dot env). Never put it in a command and never read env
  files. In Rust use `concat!(".", "env")`. Write files that contain it with the Write or Edit tools.
- Commit to your branch often, so work survives interruption. Commit messages are short plain one-liners with no
  prefixes (no `feat:`) and no co-author lines. Never push.
- Delete everything you create on the reference, the database and the VM. Never touch data you did not create.
- Comments cite what they port (`/// \`serviceParser()\``). Match the surrounding code; reuse existing helpers.
- Never commit secrets. Fixtures use a test `app.key`.
- Keep edits to shared files small (`routes/mod.rs`, `ui.rs`, `router.svelte.ts`, `App.svelte`, `api.ts`, docs); other
  branches change them too. Put new code in new modules.
- Upstream features that need something unavailable here (Stripe, a cloud provider account, a GitHub App): port what
  can be verified, test the rest with a local fake where practical, and list it in your report.
- Mind the budget: read only the PHP you need, do not dump large files into your context.
