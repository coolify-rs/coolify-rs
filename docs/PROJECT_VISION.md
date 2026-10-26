# Project Vision

coolify-rs (working name; not final) is a Rust implementation of the externally observable behaviour of
[Coolify](https://github.com/coollabsio/coolify), the self-hosted deployment platform, together with a migration
engine that keeps the Rust implementation in step with upstream development at low human cost.

## Goals

1. **Behavioural compatibility, demonstrated.** Same REST API contract, same database, same stored-secret formats,
   so coolify-rs can adopt an existing installation. "Compatible" means *covered by a differential or contract test*,
   never "looks similar".
2. **A migration system that is a first-class product.** Upstream changes are discovered, classified with evidence,
   mapped to Rust components, planned, partially generated, validated and reported automatically. Human attention is
   reserved for security-sensitive, ambiguous, destructive or unverifiable changes.
3. **An architecture that makes future ports cheap.** Contract-first boundaries (route table, model specs,
   schema guard, mapping registry) so most upstream changes land in one well-known place.
4. **Security as a release requirement.** No shell interpolation, no logged secrets, team scoping in every query,
   explicit improvements where upstream is weak (documented, never silent).

## Non-goals (for now)

- Rewriting the Livewire UI (it cannot run on a non-PHP backend; see ADR 0003).
- Fully automatic PHP→Rust translation. The engine automates what can be verified and escalates the rest.
- Claiming performance gains before measuring them.

## Definition of done for a feature

Implemented in Rust, upstream sources recorded in `compat/mapping/registry.toml`, contract tests in CI, a
differential run against the reference with zero mismatches, documented divergences, and an entry in
FEATURE_INVENTORY.md.

## Current position (2026-10-27)

Foundation, the API authentication chain, teams, projects/environments, Laravel encryption interop and the
first version of the migration engine are working and verified. 23 of 321 upstream API operations are ported.
See PROJECT_STATE.md.
