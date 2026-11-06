# coolify-rs plugin registry

This repository is the plugin registry of [coolify-rs](https://github.com/coolify-rs/coolify-rs). It lists
reviewed plugins, and its CI publishes a signed index that every coolify-rs instance reads to show its
plugin catalogue. It is not an app store. There are no accounts, uploads or ratings. A plugin gets in
through a pull request that passes the checks and a maintainer's review.
[ADR 0010, section 7](https://github.com/coolify-rs/coolify-rs/blob/main/docs/decisions/0010-plugins.md#7-distribution-a-signed-registry-not-an-app-store)
explains why.

## Layout

```
plugins/<publisher>/<name>.toml   one file per plugin, listing its versions
dist/index.json                   built from plugins/ by CI after every merge
dist/index.json.sig               Ed25519 signature of the exact bytes of index.json
scripts/                          what the workflows run
.github/workflows/review.yml      checks on every pull request
.github/workflows/publish.yml     builds and signs the index on main
```

Instances fetch `dist/index.json` and `dist/index.json.sig` from
`https://raw.githubusercontent.com/coolify-rs/plugins/main/dist`. Each instance checks the signature against
the public keys compiled into coolify-rs, and then the SHA-256 of every component it downloads.

## A plugin entry

The publisher is the GitHub user or organization that owns the source repository, so the id of a plugin
built from `github.com/acme/deploy-stats` is `acme/<name>`. `coolify-rs/` is reserved for first-party
plugins, and the publisher name `catalog` can't be used.

```toml
# plugins/acme/deploy-stats.toml

[[versions]]
version = "1.0.0"
source = "https://github.com/acme/deploy-stats"
commit = "<40-digit commit hash>"           # the exact commit; never a tag or branch
toolchain = "rust 1.99.1, target wasm32-wasip2"
builder = "docker.io/library/rust:1.99.1-slim-bookworm@sha256:<digest>"   # pinned by digest
sha256 = "<SHA-256 of the component>"
url = "https://github.com/acme/deploy-stats/releases/download/v1.0.0/deploy_stats.wasm"
# yanked = true                             # set later to stop new installs of this version

[versions.build]
dir = "."                                   # where to run the command, relative to the repository
command = "rustup target add wasm32-wasip2 && cargo build --release --locked --target wasm32-wasip2"
artifact = "target/wasm32-wasip2/release/deploy_stats.wasm"   # relative to dir

[versions.manifest]                         # plugin.toml of this version, unchanged
id = "acme/deploy-stats"
name = "Deploy stats"
version = "1.0.0"
description = "Counts deployments per team."
license = "MIT"
source = "https://github.com/acme/deploy-stats"
api = "0.1"

[versions.manifest.capabilities]
events = ["deployment.succeeded", "deployment.failed"]
storage = true
routes = true
```

A plugin that must be pulled from every instance gets a revocation at the top of its file:

```toml
[revoked]
reason = "Sends API tokens to a third party."
```

## Submitting a plugin or a new version

1. Build the component in the builder image you name, from the commit you name. The build has to be
   reproducible: CI rebuilds it and the hash must match. Run the build in `/build/src` with
   `CARGO_HOME=/build/cargo-home`, as `scripts/rebuild.sh` does, or the paths compiled into the component
   will differ.
2. Publish the component at a stable HTTPS URL, such as a GitHub release asset of the source repository.
3. Check it locally with the coolify-rs CLI:
   ```bash
   coolify-rs plugin check deploy_stats.wasm plugin.toml
   ```
4. Open a pull request that adds `plugins/<publisher>/<name>.toml`, or appends a `[[versions]]` entry to it.

Published versions never change. A later pull request can only set `yanked`, or add a revocation.
Versions and plugins are never deleted, because instances need the entry to know what they run.

## What CI checks

Every check must pass.

1. **Reproducible build.** `scripts/rebuild.sh` checks out the pinned commit, builds it in the pinned
   builder image and compares the SHA-256 with the entry. It also downloads `url` and compares that file.
   The reviewed source is then exactly what users run.
2. **Capability check.**
   - `coolify-rs plugin check` fails if the component imports an interface its manifest does not ask for.
   - `coolify-rs plugin review` prints what changed since the previous version: new capabilities, new
     HTTP hosts and new events.
3. **Conformance.** `coolify-rs plugin check` loads the component in the real sandbox. It sends every
   event kind the plugin subscribes to with a sample payload, then malformed input, and calls its routes.
   Traps, timeouts and invalid responses fail the check, and memory use and time are reported. A plugin
   may return an error for input it can't use, but it must not crash.
4. **Metadata.** `coolify-rs plugin review` checks these:
   - The license is an OSI-approved SPDX expression.
   - The publisher owns the source repository.
   - The id doesn't look like an existing id (typosquatting, judged by edit distance and look-alike
     characters).
   - The name and description don't claim an affiliation with coolify-rs or Coolify.
   - Published versions are unchanged.

The workflow takes its scripts and the coolify-rs CLI from the base branch, never from the pull request.
It has no secrets, and the submitted code runs only inside the builder image and the plugin sandbox.

## Review

After CI passes, maintainers review. `coolify-rs plugin review` prints the tier each change needs.

| Change | Review |
|---|---|
| A new plugin | One maintainer reads the source and confirms the capabilities match the stated purpose |
| A new version, same capabilities | One maintainer checks the source diff since the approved version |
| A new version with more capabilities, or any `http` host added | Two maintainers |
| Later high-risk capabilities (server commands, deployments, resource data) | Two maintainers, and a security review of every code path that uses them |

Yanking takes one maintainer. A revocation takes one maintainer, so it can happen fast. Lifting a
revocation takes two.

## Publishing and signing

When a change to `plugins/` reaches `main`, `publish.yml` runs these steps:

1. It builds `dist/index.json` with `coolify-rs plugin build-index`.
2. It signs the index with the key in the `REGISTRY_SIGNING_KEY` secret of the `registry-signing`
   environment. Give that environment required reviewers.
3. It checks the result with `coolify-rs plugin verify-index`, using the keys compiled into coolify-rs.
4. It commits `dist/`.

Instances refresh the index once a day, so a revocation reaches every instance within a day. An admin can
also refresh it from the catalogue.

### Keys

- Create a key with `coolify-rs plugin keygen registry.key registry.pub --key-id <id> --not-after <YYYY-MM-DD>`.
  The command prints only the public half.
- Store `registry.key` only as the environment secret, and delete the local copy.
- Add the public half to `PRODUCTION_KEYS` in `crates/coolify-plugins/src/registry/signing.rs` in
  coolify-rs.
- Rotation: ship the next key in a coolify-rs release well before the current key's `not_after`. Then
  switch the secret. Instances refuse signatures by unknown or expired keys.

### Setting this repository up

- Create the repository at `github.com/coolify-rs/plugins`.
- Protect `main`: require pull requests, the `review` check and maintainer approval.
- Create the `registry-signing` environment with the secret and its required reviewers.
- Set `COOLIFY_RS_REF` in both workflows to a coolify-rs commit that contains the production key.
- Replace the placeholder `sha256` of `coolify-rs/deploy-stats` with the hash from the first CI rebuild,
  and publish that component at its `url`.
