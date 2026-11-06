# ADR 0014: Per-team plugin settings

Status: proposed (2026-11-06)

## Context

The first-party plugins planned next send alerts to ntfy.sh, Google Chat, PagerDuty and Opsgenie, ping
Healthchecks.io, chain deployments and report delivery metrics. Each needs values that differ per team: a
topic, a routing key, an API key, a webhook URL. Some of them are secrets. Plugins had no way to receive
configuration: `storage` is written by the plugin itself, and the manifest is the same for every team.

## Decision

### 1. Declared in the manifest

```toml
[[settings]]
key = "routing_key"           # lowercase snake case, unique
label = "Routing key"
help = "The integration key of a PagerDuty service."
secret = true                 # encrypted at rest, never shown again
required = true               # "Needs configuration" until set

[[settings]]
key = "severity"
label = "Severity"
options = ["info", "warning", "error", "critical"]   # a select
default = "error"
```

Fields: `key`, `label`, optional `help`, `secret`, `required`, optional `default` (not for secrets, not with
`required`), optional `options` (a select; not for secrets), optional `kind` = `text` | `url` | `select`
(`url` values must be `https://` URLs with a host and no credentials). Unknown fields are refused, as
everywhere in the manifest. At most 32 settings; labels up to 64 characters, help up to 280, values up to
4096 bytes without control characters.

**Settings are not a capability.** They give the plugin nothing it could not already do; they only let the
team hand it values. The capability summary an admin approves does not mention them, so grants made before
settings existed still match and a version that adds settings is not an escalation. Reviewers still read
them: a setting called `password` on a plugin with `http` deserves a question.

### 2. The `config` interface (`coolify-rs:plugin@0.1.2`)

```wit
interface config {
    get: func(key: string) -> option<string>;
}
```

The host takes the plugin and the team from the call, like every interface: a plugin reads its own settings
for the call's team and nothing else. `get` answers the team's value, else the default, else none; also none
for a key the manifest does not declare. A required setting that is unset is none too, and plugins must
handle that (skip the alert and log it). There is no `all`: a plugin knows its keys.

Only a component whose manifest declares settings may import `config`; any other is refused at install and
at start, like an import outside the grant. 0.1.2 is additive: components built for 0.1.0 and 0.1.1 load
unchanged (`plugins/testing/compat-0-1-0` and `compat-0-1-1` are frozen to prove it).

### 3. Storage

`coolify_rs_plugins.team_settings (plugin_id, team_id, key, value, secret, updated_by, updated_at)`. A
secret's value is `Encrypter::encrypt_string` of it: Laravel's AES-256-CBC payload under `APP_KEY`, the same
encrypter and key coolify-rs uses for Coolify's encrypted columns (private keys, database passwords).
coolify-plugins depends on coolify-compat for the type; coolify-api hands its encrypter to the host when it
connects the bridges, so there is one key and no second configuration. Without `APP_KEY` a secret can't be
saved (the API says so) and reads as none.

- **Uninstall** deletes every team's values (foreign key with `ON DELETE CASCADE`).
- **A team switching the plugin off keeps them**, so switching it on again does not mean typing secrets
  again. Deleting them is a Clear in the dialog.
- **An update reconciles them** with the new declarations: a key that is gone is deleted; a value that turned
  secret is encrypted in place; a secret that is no longer secret is deleted (it was entered as a secret, and
  members would now see it); a value the new declaration refuses (another kind, other options) is deleted. A
  secret that can't be decrypted is kept, so a key problem never destroys data. The audit log records what
  happened by team and key.

### 4. API and web UI

- `GET /api/v1/plugins/{publisher}/{name}/settings` (`read`): every declared setting with `set`; `value` only
  for settings that are not secret; and `needs_configuration`. Any member of the token's team.
- `PATCH` the same path (`write`, team owners and admins): a JSON object of `key: string | null`. A string sets,
  null or a blank string clears, keys left out stay. Everything is validated before anything is written;
  errors are the API's usual 422 `{"message": "Validation failed.", "errors": {key: [message]}}`, and no
  message repeats a value. Secrets are write-only: no answer ever contains one.
- Every change is written to the plugin audit log as `team_settings_updated` with `{"set": [keys], "cleared":
  [keys]}` and the user, never values.
- The session equivalents are `GET`/`PATCH /ui/plugins/{publisher}/{name}/settings`, and each plugin on
  `GET /ui/plugins/team` carries `settings` and `needs_configuration`.
- Team → Plugins shows **Configure** for a plugin with settings: Coolify's modal and form fields. A secret's
  field is empty with a "••••• (set)" placeholder; leaving it empty keeps the secret, and Clear removes it.
  A **Needs configuration** badge shows while a required setting is unset. Members see the dialog read-only,
  without secrets.

## Threat model

| Who | Can read a secret? |
|---|---|
| The plugin, for the call's team | **Yes, by design.** It needs the API key to call the service. A plugin with `http` can send it to its allowed hosts, which is the point; the registry review and the admin's grant are the controls for what the plugin does with it, as for all plugin code. |
| The same plugin, for another team | No. The team comes from the call; no function takes a team. |
| Another plugin | No. The plugin id comes from the host's call context; `config` has no plugin parameter. |
| Team members, owners and admins | No. They see whether it is set. Owners and admins can replace or clear it. |
| Instance admins through the API or UI | No. They can uninstall the plugin, which deletes it. |
| Logs and the audit log | No. Nothing logs a value; validation messages name the setting, not the value; audits list keys. |
| Someone with a database dump | Not without `APP_KEY`, as for Coolify's own encrypted columns. |

What remains: a plugin can leak a secret it was given (log it, return it from a route, send it to an allowed
host). That is a property of the plugin's code, reviewed in the registry like every other behaviour, and the
SDK documents that secrets must not be logged or returned.

## Consequences

- Alert plugins can ship as plain WebAssembly components with no out-of-band configuration.
- `APP_KEY` rotation follows Coolify's: secrets encrypted under a previous key decrypt while it is listed in
  `APP_PREVIOUS_KEYS`; re-saving a setting re-encrypts it under the current key.
- `coolify-rs plugin check` runs plugins with defaults only, so a plugin that crashes on an unset setting
  fails the check.
- Upstream Coolify has no plugins: with no plugin installed nothing changes, and the table lives in the
  `coolify_rs_plugins` schema Coolify never reads (ADR 0010, section 5).
