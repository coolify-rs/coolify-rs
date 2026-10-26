# ADR 0001: Workspace layout and dependency policy

Status: accepted (2026-10-27)

## Decision

A Cargo workspace with five library crates and two binaries, introduced only where a boundary pays for itself:

| Crate | Responsibility |
|---|---|
| `coolify-domain` | pure types and rules (roles, abilities, naming patterns, ids); no I/O |
| `coolify-compat` | upstream storage/wire formats: Laravel encrypter, Sanctum tokens, PHP JSON, PHP serialized strings |
| `coolify-db` | adopted upstream Postgres schema: schema guard, model specs, repositories |
| `coolify-api` | the `/api` HTTP contract: middleware chain, validation, serialization, handlers |
| `coolify-port` | the upstream migration engine (no dependency on the application crates) |
| `apps/coolify-rs` | server binary |
| `apps/coolify-port` | engine CLI (`coolify-port`) |

The proposed 20-crate layout in the brief was not created: crates are added when a slice needs them
(e.g. `remote-exec` with milestone 4).

Toolchain pinned to Rust 1.95.0 (`rust-toolchain.toml`), edition 2024, `Cargo.lock` committed.

Dependencies: tokio, axum 0.8, tower-http, sqlx 0.8 (runtime queries; no compile-time DB needed), serde/serde_json
(`preserve_order` for Laravel key order), tracing, clap, thiserror/anyhow, regex, chrono. RustCrypto `aes 0.8`,
`cbc 0.1`, `hmac 0.12`, `sha2 0.10`, `rand 0.8`: newer major versions exist (`aes 0.9`, `sha2 0.11`, `rand 0.10`);
the current lines were chosen for API stability and are still maintained. Upgrading is tracked as a chore; the
interop fixtures make the upgrade safe to verify.

Rules: `unsafe_code = forbid`; clippy `unwrap_used`/`expect_used` warn outside tests; CI denies warnings; `cargo
audit` and gitleaks run in CI. Git is driven through the `git` CLI with argv arrays instead of `gix` to keep the engine
small and its behaviour identical to what humans run.
