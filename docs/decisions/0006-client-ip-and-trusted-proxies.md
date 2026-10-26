# ADR 0006: Client IP resolution diverges from upstream

Status: accepted (2026-10-27)

## Context

Upstream trusts `X-Forwarded-For` from any peer (`TrustProxies::$proxies = '*'`), so a client that can reach the app
directly can choose the IP the API allowlist checks.

## Decision

coolify-rs uses the TCP peer address unless `COOLIFY_RS_TRUSTED_PROXIES` lists trusted proxy IPs/CIDRs; then it takes
the rightmost untrusted hop. `*` restores upstream behaviour for operators who need it.

## Consequences

Behind a reverse proxy, operators must configure the proxy address, otherwise the allowlist sees the proxy's IP.
Documented in KNOWN_LIMITATIONS.md and the README.
