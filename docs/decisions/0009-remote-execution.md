# ADR 0009: Remote execution over OpenSSH

Status: accepted (2026-10-27)

## Context

Everything Coolify does to a managed server goes through `instant_remote_process` / `remote_process`
(`bootstrap/helpers/remoteProcess.php`, `app/Helpers/SshMultiplexingHelper.php`): it builds one shell string,

```
timeout N ssh [mux options] -i <key> -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
  -o PasswordAuthentication=no -o ConnectTimeout=C -o ServerAliveInterval=20 -o RequestTTY=no \
  -o LogLevel=ERROR -p <port> '<user>'@'<ip>' 'sh -c "...exec bash -se..."' << \DELIM
<script>
DELIM
```

and runs it with the PHP process's local shell. Keys are written once to `storage/app/ssh/keys/ssh_key@<uuid>`
(mode 0600) and stay there. Non-root users get `sudo` inserted by `parseCommandsByLineForSudo`. Failures throw
the trimmed stderr, or `SSH command failed with exit code: N`; timeouts become
`The command on the server timed out after N seconds.` (code 124). `SshRetryable` retries up to 3 attempts with
`min(2 * 2^attempt, 30)` s backoff, only when the exit code is 255 (or not positive) and stderr matches a fixed list.

Server validation (`ValidateServer`, `Server::validate*`) runs about ten such commands and writes
`server_settings.is_reachable`, `is_usable`, `docker_version`, `compose_version` and `servers.validation_logs`.

This is the first code in coolify-rs that runs processes and talks to hosts the user controls, so the security
review comes first. The threat model is in SECURITY_MODEL.md section 8; this ADR records the design.

## Decision

1. **New crate `coolify-remote`.** It owns process spawning, key handling, host keys, timeouts, retries and the
   sudo rewrite. No other crate spawns `ssh`. It depends on `coolify-domain`-level types only, not on the database.
2. **System OpenSSH, spawned with an argv array.** Upstream's behaviour and its user-visible error strings come from
   OpenSSH, so a pure-Rust SSH client would change messages users see in `validation_logs`. coolify-rs runs
   `ssh` directly with `tokio::process::Command` (no local shell, no `timeout` binary, no heredoc). The remote
   bootstrap `sh -c "if command -v bash ...; then exec bash -se; else exec sh -se; fi"` is the same, and the script
   is written to the child's stdin, so remote semantics match upstream.
3. **Argument hygiene.** `-l <user> -p <port> -- <host>`; the execution layer refuses users and hosts that start with
   `-` or contain whitespace or control characters, and ports outside 1–65535, even though the API already
   validates them (rows can come from upstream or older versions). `-F /dev/null` ignores the operator's SSH
   config; the child environment is cleared except `PATH` and a private `HOME`, so database credentials and
   `APP_KEY` never reach `ssh` or a `ProxyCommand`.
4. **Hardened options.** Upstream's options, plus `BatchMode=yes`, `IdentitiesOnly=yes`, `IdentityAgent=none`,
   `ForwardAgent=no`, `ForwardX11=no`, `ClearAllForwardings=yes`, `PermitLocalCommand=no`, `UpdateHostKeys=no`.
   The Cloudflare tunnel `ProxyCommand=cloudflared access ssh --hostname %h` is kept as upstream has it.
5. **Host keys: trust on first use, pinned per server.** `StrictHostKeyChecking=accept-new` with
   `UserKnownHostsFile=<data>/ssh/known_hosts/<server uuid>` and `GlobalKnownHostsFile=/dev/null`. A changed key
   fails with `Host key verification failed: the server's SSH host key changed ...`, which names the new fingerprint
   and the way forward instead of OpenSSH's banner (that banner names the pin file, a control-plane path), and is not
   retried. The pin is dropped when the server's IP or port changes and when the server is deleted; at startup, pins of
   servers that no longer exist (deleted by upstream on a shared database) are removed. `COOLIFY_RS_SSH_HOST_KEYS=ignore` restores upstream's behaviour.
6. **Keys never persist.** The encrypted `private_keys.private_key` is decrypted right before a run into a
   zeroizing buffer, written to a random-named file created with mode 0600 inside `<data>/ssh/keys` (mode 0700),
   and removed when the run ends. Leftovers from a crash are removed at startup. Key material has no `Debug`.
7. **Bounded runs.** Every run has a deadline (upstream's per-call timeout; `SSH_COMMAND_TIMEOUT`, default 3600 s);
   on expiry the child is killed and the error is upstream's message, which never contains the command.
   stdout and stderr are capped (16 MiB / 1 MiB; upstream has no cap) and decoded lossily as UTF-8. A global
   semaphore limits concurrent SSH processes (`COOLIFY_RS_SSH_MAX_CONCURRENCY`, default 32).
8. **Upstream semantics kept.** Output is trimmed and `"null"` becomes no output; the error rules, the retry
   pattern list, the backoff and the attempt counts (validation's connection check uses one attempt) are ported
   as they are. `parseCommandsByLineForSudo` is ported with upstream's unit tests as the spec.
9. **No multiplexing yet.** Each run opens its own connection (upstream's validation connection check also
   disables it). ControlMaster sockets bring their own lifecycle and permission questions; they come with the
   deployment engine, which needs them for performance.
10. **Background jobs in-process.** `POST /servers/{uuid}/validate` answers 201 and runs `ValidateServer` on a
    tokio task, like upstream's queued job. One validation per server runs at a time; a second request while one
    runs is accepted and does nothing. Jobs do not survive a restart (upstream's Redis queue does).
11. **Audit.** Each run logs (tracing target `audit`) the server uuid, a fixed operation name, attempt, exit code
    and duration. Scripts and outputs are never logged. The `audit_events` table is still not written by
    coolify-rs (KNOWN_LIMITATIONS).
12. **Not ported.** `install=true` (installing prerequisites and Docker, starting the proxy) answers 501, like
    other unported side effects. The Coolify Cloud validation throttle is not ported (coolify-rs is self-hosted
    only). Cloud provider state checks (Vultr, DigitalOcean, Hostinger) are skipped for servers without provider
    IDs; servers that have them answer 501.

## Consequences

- Validation results and `validation_logs` match upstream for the same host, except where a pinned host key
  changed (upstream connects anyway).
- Operators whose servers legitimately change host keys must re-enter the IP/port, delete the pin file, or opt out.
- The control plane needs an OpenSSH client (`ssh` on `PATH`), version 7.6 or newer for `accept-new`.
- A data directory is required: `COOLIFY_RS_DATA_DIR` (default `/data/coolify-rs`).
