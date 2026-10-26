# Migration Strategy

Two migrations are in play: (1) porting Coolify's behaviour to Rust in vertical slices, and (2) continuously porting
*future* upstream changes. This document covers both; the engine is `crates/coolify-port` + `apps/coolify-port`.

## 1. Vertical slices

A slice is complete only with: Rust domain/persistence/API code, mapping entry, contract tests (CI), differential
run against the reference (0 mismatches), documented divergences. Order follows dependencies and risk
(IMPLEMENTATION_ROADMAP.md). The first slice (auth chain + teams + projects/environments) was chosen because it
exercises every architectural assumption (token auth, team scoping, policies, validation messages, serialization,
schema adoption, writes with model side effects) without host-level privileges.

## 2. Pipeline overview

```
sync ─► track ─► analyse ─► classify ─► map ─► plan ─► generate (A/B) ─► validate ─► repair (bounded) ─► report
 git      state    diff,      paths +     registry   risk,     isolated        5-state     external      ledger,
 mirror   file     PHP AST-   content +   query      actions,  branch +        outcomes    agent (off    metrics,
                   lite       message                escal.    worktree                    by default)   prompt pack
```

Commands:

```bash
coolify-port sync                       # fetch the bare mirror
coolify-port status                     # pending commits since last analysed / integrated
coolify-port run [--from R] [--to R]    # analyse + plan (no side effects outside .migration-runs)
coolify-port run --generate             # + layer A patch in branch port/<run>, validation, repair, report
coolify-port mark-integrated --run-id   # a human records that the port was merged
```

## 3. Tracking

- Upstream is a bare mirror (`.upstream/coolify.git`); the engine passes `--git-dir` explicitly and never touches a
  working tree. Revisions are validated (no option injection) and git runs with argv arrays, no pager, no prompts.
- `migration/state.json` keeps `baseline`, `last_analyzed`, `last_integrated` and a history. Markers only move forward
  along ancestry; ad-hoc older ranges are recorded as `analyzed-subrange`.
- Units of change are first-parent commits (merges count once). Runs have deterministic ids
  (`run-<from>-<to>`) so re-running a range resumes it.

## 4. Classification and mapping

Evidence kinds (all recorded per category):

- **path**: ordered rules (`PATH_RULES`) map files to 17 categories; security paths raise signals;
- **content**: changed lines only: `Route::` in route files, `Schema::` in migrations, security-relevant tokens
  (`authorize(`, `escapeshellarg`, `Crypt::`, `api.ability`, …), changed PHP symbols from hunks;
- **message**: conventional-commit type; weak; **never** sufficient alone (a `docs:` commit touching a policy is
  classified as authorization).

Language-aware analysis available today: a PHP tokenizer, a Laravel route extractor (verified identical to
`route:list` on 321 routes) and a migration summarizer (`up()` ops). A full PHP AST (e.g. nikic/php-parser run in an
isolated container) is a planned upgrade.

The registry (`compat/mapping/registry.toml`) links upstream paths, symbols, routes, tables and tests to Rust paths,
tests and packages, with status and divergences. It answers "which Rust parts are affected by these upstream changes?"
using explicit metadata, not file-name similarity. Planned components are listed too so classification coverage can
be measured.

## 5. Planning and risk

| Risk | Triggers (impact-based, not size-based) |
|---|---|
| High | security signals; auth/authorization; migrations touching tables used by implemented components; destructive/raw-SQL/encryption migrations; deployment/container paths mapped to implemented components |
| Medium | API changes, domain logic, schema changes on unmapped tables, jobs, dependency upgrades, integrations, configuration |
| Low | documentation, tests, frontend, service templates, localization, build/release |

Each plan lists actions by layer:

- **A** deterministic regeneration (route inventory, supported migrations);
- **B** template-driven changes (e.g. register a reviewed optional column in a model spec);
- **C** AI-assisted semantic port (prompt pack + isolated worktree + validation gates);
- **D** human-led (all high-risk component changes).

High risk always escalates with: reasons, affected components, decision required.

## 6. Validation

Checks come from the plan (fmt, clippy, tests of mapped packages, or the whole workspace). Only allowlisted programs
run. Outcomes are `passed | failed | skipped | blocked | inconclusive`; missing infrastructure (e.g. the test
database URL listed in `validation.required_env`) yields `inconclusive`, never `passed`. A failed required check
blocks the rest and marks the patch `failed`.

The API contract tests include `route_inventory.rs`, which compares every ported route with the regenerated upstream
inventory, and the schema guard test, so layer A regeneration directly exercises compatibility.

## 7. Bounded repair

Configured in `port.toml [repair]` (`max_attempts`, `agent_command`, `agent_timeout_secs`). The agent runs with a
cleared environment (only `PATH`, `HOME`, `PORT_RUN_DIR`, `PORT_PROMPT_PACK`), inside the run worktree, with a
timeout. After each attempt the diff is scanned for test tampering (removed `#[test]`, added `#[ignore]`, removed
assertions, edits to `compat/baseline/`, fixtures or CI); any finding fails the attempt. **The agent hook is disabled
by default**; enabling it is a security decision (SECURITY_MODEL.md §5).

## 8. Outcome tracking and calibration

`migration/ledger/metrics.json` records per run: commits, risk, escalation, verdict, first-attempt validation,
repair attempts, security-sensitive commits, and a human `review_outcome`. Classification accuracy is **not yet
calibrated**: the "mapping coverage" figure in plans is a heuristic, not a confidence. Calibration plan: label a sample
of ~200 historical upstream commits by hand (categories, risk), measure precision/recall per category, and only then
report confidence values.

## 9. Results so far (ledger)

| Run | Range | Commits | Risk | Verdict | What it showed |
|---|---|---|---|---|---|
| `run-5ca69705d-b5823c777` | v4.5.0..main | 148 | High (escalated) | failed | Detected 4 new migrations, two on mapped tables (`projects.canvas_position`, `teams.is_two_factor_required`); detected a real security-relevant change in a verified component (`80ce0efe7`: API allowlist now honours `CF-Connecting-IP` from Cloudflare peers). Layer A patch regenerated the supported-migration list; validation then failed because the schema guard and test baseline still describe 4.5.0. That is the correct outcome: accepting the new schema needs a reviewed baseline update (layer B/D). **Resolved by a human-led port (session 2):** baseline moved to `main` @ b5823c7, migrations accepted, Cloudflare client IP ported; tests 86/86, differential 102/102; recorded with `mark-integrated` (`review_outcome = accepted-after-manual-port`). |
| `run-b17c83d68-a1c24e084` | 1 commit | 1 | Low | validated | Frontend-only change, nothing to port, full workspace validation passed. |
| `run-f75136586-4559d21b4` | 1 commit | 1 | Medium | validated | API fix in an unported controller (`ApplicationsController`); correctly classified, no Rust impact, coverage 0% reported. |

What the resolution took, for effort accounting: capturing the new runtime schema (upstream image), switching
the test baseline, porting one security-relevant behaviour (`client_ip()`), two new contract tests and five new
differential cases. The two schema changes needed no code because the columns had been pre-registered after the
engine's first report, which is the layer B pattern to automate next.

During development the pipeline also blocked two of our own regressions (clippy and rustfmt failures in the worktree),
demonstrating that validation gates are enforced. Human-effort reduction has **not** been measured yet: no
representative semantic port has been run through the pipeline (see roadmap).

## 10. Data migration of existing installations

coolify-rs adopts the upstream database in place (no data copy, no DDL). The supported procedure:

1. Back up the upstream database (`pg_dump`) and `APP_KEY`.
2. Run `coolify-rs check-schema` against it; unknown or missing migrations mean the versions do not match.
3. Run coolify-rs side-by-side (API only) with the same `DB_*` settings; it starts read-only if the schema is newer.
4. Never delete the upstream installation; rollback is "stop coolify-rs".

Consistency caveats of side-by-side operation are in DATABASE_COMPATIBILITY.md §6.
