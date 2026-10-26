# Security Policy

coolify-rs is pre-release software and must not be exposed to untrusted networks yet.

Report vulnerabilities privately to the maintainers (contact to be published with the first release); do not open
public issues for security problems.

In scope: authentication/authorization bypass, cross-team data access, secret disclosure (including through logs),
command injection, SSRF, and weaknesses in the migration engine's isolation (e.g. upstream content influencing
execution). The threat model and controls are documented in [docs/SECURITY_MODEL.md](docs/SECURITY_MODEL.md).

Vulnerabilities in upstream Coolify should be reported to the Coolify maintainers per their own policy.
