//! `App\Actions\Server\AuthorizedKeys` (upstream v4.7.5): adds and removes public keys in
//! `~/.ssh/authorized_keys` of the SSH user. Both operations are safe to run again.

use std::net::IpAddr;

use coolify_compat::sshkey;
use coolify_remote::RunOptions;

use super::{Srv, shell_arg};

/// The exit code of `add()` when `~/.ssh/authorized_keys` has no line with the old key.
pub const OLD_KEY_NOT_FOUND: i32 = 3;

/// The awk program of `rotatedLinesCommands()`: it tokenizes outside quoted options and never
/// interprets options as shell commands.
const ROTATED_LINES_AWK: &str = r##"function token(    start,c,quoted,escaped) {
    while (substr($0,pos,1) ~ /[ \t]/ && pos <= length($0)) pos++
    start=pos; quoted=0; escaped=0
    while (pos <= length($0)) {
        c=substr($0,pos,1)
        if (escaped) escaped=0
        else if (c == "\\" && quoted) escaped=1
        else if (c == "\"") quoted=!quoted
        else if (!quoted && c ~ /[ \t]/) break
        pos++
    }
    return substr($0,start,pos-start)
}
{
    pos=1
    first=token()
    if (first == "" || substr(first,1,1) == "#") next
    prefix=""
    if (first != type) { prefix=first " "; first=token() }
    data=token()
    if (first == type && data == key) { print prefix replacement; found=1 }
}
END { if (!found) exit 1 }"##;

/// An `instant_remote_process` failure with the exit code of ssh or of the script.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyEditError {
    pub message: String,
    pub code: Option<i32>,
}

impl KeyEditError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            code: None,
        }
    }
}

/// `AuthorizedKeys::canRunOn()`.
pub fn can_run_on(srv: &Srv) -> bool {
    !srv.info.transferred && !srv.info.skipped()
}

/// `rotatedLinesCommands()`: writes to `output` the lines that `add()` writes: the new key with
/// the options of each line of the old key. Runs `not_found` when the file has no line with the
/// old key.
fn rotated_lines_commands(
    new_line: &str,
    old_body: &str,
    output: &str,
    not_found: &str,
) -> Vec<String> {
    vec![
        format!("old={}", shell_arg(old_body)),
        "type=${old%% *}; key=${old#* }".to_owned(),
        format!(
            "awk -v type=\"$type\" -v key=\"$key\" -v replacement={} {} \"$f\" > {output} || {not_found}",
            shell_arg(new_line),
            shell_arg(ROTATED_LINES_AWK)
        ),
    ]
}

/// The commands of `AuthorizedKeys::add()`: the new key with the options of every line of the
/// old key. The old key stays until `remove()`.
pub fn add_commands(new_line: &str, old_body: &str) -> Vec<String> {
    let mut commands = vec![
        r#"f="$HOME/.ssh/authorized_keys""#.to_owned(),
        r#"tmp=$(mktemp "$f.cpk-XXXXXX")"#.to_owned(),
    ];
    commands.extend(rotated_lines_commands(
        new_line,
        old_body,
        r#""$tmp""#,
        &format!(
            r#"{{ echo "The old key is not in ~/.ssh/authorized_keys of the SSH user." >&2; exit {OLD_KEY_NOT_FOUND}; }}"#
        ),
    ));
    // A last line without a final newline would be joined with the new line.
    commands
        .push(r#"if [ -s "$f" ] && [ -n "$(tail -c1 "$f")" ]; then echo >> "$f"; fi"#.to_owned());
    // Do not add an unrestricted fallback, including on retry.
    commands.push(
        r#"while IFS= read -r entry; do grep -qxF -- "$entry" "$f" || printf '%s\n' "$entry" >> "$f"; done < "$tmp""#
            .to_owned(),
    );
    commands
}

/// The commands of `AuthorizedKeys::undoAdd()`: removes only the exact lines that `add()` wrote.
pub fn undo_add_commands(new_line: &str, old_body: &str) -> Vec<String> {
    let mut commands = vec![
        r#"f="$HOME/.ssh/authorized_keys""#.to_owned(),
        r#"if [ ! -f "$f" ]; then exit 0; fi"#.to_owned(),
        r#"tmp=$(mktemp "$f.cpk-XXXXXX")"#.to_owned(),
        r#"cp -p "$f" "$tmp""#.to_owned(),
    ];
    // Without a line of the old key, add() wrote nothing.
    commands.extend(rotated_lines_commands(
        new_line,
        old_body,
        r#""$tmp.lines""#,
        "exit 0",
    ));
    commands.extend(
        [
            "rc=0",
            r#"grep -vxF -f "$tmp.lines" -- "$f" > "$tmp" || rc=$?"#,
            r#"if [ "$rc" -gt 1 ]; then exit 1; fi"#,
            r#"mv -f "$tmp" "$f""#,
        ]
        .map(str::to_owned),
    );
    commands
}

/// The commands of `AuthorizedKeys::removePublicKey()`: removes every line with the public key,
/// also lines that cloud-init or a cloud provider added.
pub fn remove_commands(public_key_body: &str) -> Vec<String> {
    vec![
        r#"f="$HOME/.ssh/authorized_keys""#.to_owned(),
        r#"if [ ! -f "$f" ]; then exit 0; fi"#.to_owned(),
        r#"tmp=$(mktemp "$f.cpk-XXXXXX")"#.to_owned(),
        // Preserve the existing owner and mode before replacing the file atomically.
        r#"cp -p "$f" "$tmp""#.to_owned(),
        "rc=0".to_owned(),
        format!(
            r#"grep -vF -- {} "$f" > "$tmp" || rc=$?"#,
            shell_arg(public_key_body)
        ),
        r#"if [ "$rc" -gt 1 ]; then exit 1; fi"#.to_owned(),
        r#"mv -f "$tmp" "$f""#.to_owned(),
    ]
}

/// `runLocked()`: all edits share a lock in the remote SSH home, also when server records use
/// different aliases or keys. mkdir is atomic and available on supported hosts without flock.
pub fn locked_commands(commands: Vec<String>) -> Vec<String> {
    let mut all: Vec<String> = [
        r#"if [ ! -d "$HOME/.ssh" ]; then mkdir -p "$HOME/.ssh" && chmod 700 "$HOME/.ssh"; fi"#,
        r#"lock="$HOME/.ssh/.coolify-authorized-keys.lock""#,
        "attempt=0",
        // A lock older than 10 minutes is left from an edit that crashed. Edits take a few seconds.
        r#"find "$lock" -maxdepth 0 -type d -mmin +10 -exec rmdir {} \; 2>/dev/null || true"#,
        r#"until mkdir "$lock" 2>/dev/null; do"#,
        "attempt=$((attempt + 1))",
        r#"if [ "$attempt" -ge 30 ]; then echo "Another authorized_keys edit is running; try again later." >&2; exit 1; fi"#,
        "sleep 1",
        "done",
        r#"tmp="""#,
        r#"trap '[ -z "$tmp" ] || rm -f "$tmp" "$tmp.lines"; rmdir "$lock"' EXIT"#,
        "trap 'exit 1' HUP INT TERM",
    ]
    .map(str::to_owned)
    .to_vec();
    all.extend(commands);
    all
}

/// `instant_remote_process($commands, $server, no_sudo: true)` under the lock.
async fn run_locked(srv: &Srv, commands: Vec<String>) -> Result<(), KeyEditError> {
    let target = srv.target().await.map_err(KeyEditError::new)?;
    let commands = locked_commands(commands);
    let refs: Vec<&str> = commands.iter().map(String::as_str).collect();
    srv.jobs
        .remote()
        .run(
            &target,
            &refs,
            RunOptions::op("server.authorized_keys").no_sudo(),
        )
        .await
        .map(|_| ())
        .map_err(|e| KeyEditError {
            message: e.message,
            code: e.code,
        })
}

fn assert_can_run_on(srv: &Srv) -> Result<(), KeyEditError> {
    if can_run_on(srv) {
        Ok(())
    } else {
        Err(KeyEditError::new(
            "Coolify does not run commands on this server.",
        ))
    }
}

/// `AuthorizedKeys::add($server, $privateKey, $oldKey)`. Fails with the code
/// [`OLD_KEY_NOT_FOUND`] when the file has no line with the old key.
pub async fn add(srv: &Srv, new_line: &str, old_body: &str) -> Result<(), KeyEditError> {
    assert_can_run_on(srv)?;
    run_locked(srv, add_commands(new_line, old_body)).await
}

/// `AuthorizedKeys::undoAdd()`: undoes `add()` after the new key could not log in. Other lines
/// stay, also lines that contain the new key. Returns false when another server record for the
/// same host and user uses the new key.
pub async fn undo_add(
    srv: &Srv,
    new_line: &str,
    new_body: &str,
    new_key_id: i64,
    old_body: &str,
) -> Result<bool, KeyEditError> {
    assert_can_run_on(srv)?;
    if is_used_on_same_host(srv, new_body, Some(new_key_id)).await? {
        return Ok(false);
    }
    run_locked(srv, undo_add_commands(new_line, old_body)).await?;
    Ok(true)
}

/// `AuthorizedKeys::removePublicKey()`. Returns false when another server record for the same
/// host and user still uses the key.
pub async fn remove_public_key(
    srv: &Srv,
    public_key_body: &str,
    private_key_id: Option<i64>,
) -> Result<bool, KeyEditError> {
    assert_can_run_on(srv)?;
    if is_used_on_same_host(srv, public_key_body, private_key_id).await? {
        return Ok(false);
    }
    run_locked(srv, remove_commands(public_key_body)).await?;
    Ok(true)
}

/// `AuthorizedKeys::remove($server, $server->privateKey)`: removes every line with the public
/// key of the server's own SSH key. Returns false when another server record for the same host
/// and user still uses the key.
pub async fn remove_server_key(srv: &Srv) -> Result<bool, KeyEditError> {
    let key_id = srv
        .info
        .private_key_id
        .ok_or_else(|| KeyEditError::new("The private key is invalid."))?;
    let key = coolify_db::private_keys::find_by_id(srv.db(), srv.info.team_id, key_id)
        .await
        .map_err(|e| KeyEditError::new(e.to_string()))?
        .ok_or_else(|| KeyEditError::new("The private key is invalid."))?;
    let plain = srv
        .jobs
        .encrypter()
        .ok_or_else(|| KeyEditError::new("No application encryption key has been specified."))?
        .decrypt_string(&key.private_key_encrypted)
        .map_err(|_| KeyEditError::new("The private key is invalid."))?;
    let body = sshkey::public_key_body(&plain)
        .ok_or_else(|| KeyEditError::new("The private key is invalid."))?;
    remove_public_key(srv, &body, Some(key_id)).await
}

/// `Server::normalizeIp()` as far as this comparison needs it: the allowed characters, in lower
/// case, without a trailing dot, and an IP address in its canonical form.
fn normalize_host(value: &str) -> String {
    let cleaned: String = value
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || ".:%-".contains(*c))
        .collect();
    let host = cleaned.trim_end_matches('.').to_lowercase();
    match host.parse::<IpAddr>() {
        Ok(ip) => ip.to_string(),
        Err(_) => host,
    }
}

/// `hostKeys()`: the pinned SSH host keys of a server, as "type base64" lines.
fn host_keys(pinned: Option<&str>) -> Vec<String> {
    pinned
        .unwrap_or_default()
        .lines()
        .map(|line| {
            line.split_whitespace()
                .take(2)
                .collect::<Vec<_>>()
                .join(" ")
        })
        .filter(|line| !line.is_empty())
        .collect()
}

/// `hostAddresses()`: an IP address is its own address. Upstream resolves a hostname with the
/// first custom DNS server of the instance and treats a name it cannot resolve as unknown;
/// coolify-rs resolves no names here, so a hostname is always unknown and the key is kept.
fn host_addresses(host: &str) -> Vec<String> {
    match host.parse::<IpAddr>() {
        Ok(ip) => vec![ip.to_string()],
        Err(_) => Vec::new(),
    }
}

/// The decision of `isUsedOnSameHost()` for one other server record that uses the key: true
/// when it cannot be shown that the two records are different machines.
fn may_be_same_machine(
    host: &str,
    other_host: &str,
    host_keys_here: &[String],
    host_keys_there: &[String],
) -> bool {
    if host == other_host {
        return true;
    }
    // Two addresses of one machine (for example a public and a Tailscale IP) show the same host
    // key. Without a pinned host key on both records, it is not known that these are different
    // machines.
    if host_keys_here.is_empty()
        || host_keys_there.is_empty()
        || host_keys_here.iter().any(|k| host_keys_there.contains(k))
    {
        return true;
    }
    let here = host_addresses(host);
    let there = host_addresses(other_host);
    // An unresolved hostname cannot prove that these are different machines. Keep the key.
    here.is_empty() || there.is_empty() || here.iter().any(|a| there.contains(a))
}

/// `AuthorizedKeys::isUsedOnSameHost()`.
async fn is_used_on_same_host(
    srv: &Srv,
    public_key_body: &str,
    private_key_id: Option<i64>,
) -> Result<bool, KeyEditError> {
    let fingerprint = sshkey::public_key_fingerprint(public_key_body);
    // Other teams count only with the same key material: removing it would lock them out.
    // Keys saved before fingerprints existed have none stored, so compare their key material.
    type Other = (
        Option<String>,
        Option<i64>,
        Option<String>,
        Option<String>,
        Option<String>,
    );
    let others: Vec<Other> = sqlx::query_as(
        "SELECT s.ip::text, s.private_key_id::bigint, pk.fingerprint::text, pk.private_key::text, \
         (SELECT ss.ssh_host_key FROM server_settings ss WHERE ss.server_id = s.id LIMIT 1) \
         FROM servers s LEFT JOIN private_keys pk ON pk.id = s.private_key_id \
         WHERE s.deleted_at IS NULL AND s.\"user\" = $1 AND s.id <> $2 \
         AND (($3::bigint IS NOT NULL AND s.private_key_id = $3) \
              OR (pk.id IS NOT NULL AND (pk.fingerprint = $4 OR pk.fingerprint IS NULL)))",
    )
    .bind(&srv.info.user)
    .bind(srv.id())
    .bind(private_key_id)
    .bind(fingerprint.as_deref())
    .fetch_all(srv.db().pool())
    .await
    .map_err(|e| KeyEditError::new(e.to_string()))?;
    let host = normalize_host(&srv.info.ip);
    let here = host_keys(
        srv.row
            .settings
            .as_ref()
            .and_then(|s| s.get("ssh_host_key"))
            .and_then(serde_json::Value::as_str),
    );
    for (ip, other_key_id, other_fingerprint, other_private_key, other_host_key) in others {
        let same_record_key = private_key_id.is_some() && other_key_id == private_key_id;
        let same_material = || {
            let wanted = fingerprint.as_deref()?;
            let stored = other_fingerprint
                .clone()
                .filter(|f| !f.is_empty())
                .or_else(|| {
                    // `PrivateKey::generateFingerprint($other->privateKey->private_key)`.
                    let plain = srv
                        .jobs
                        .encrypter()?
                        .decrypt_string(other_private_key.as_deref()?)
                        .ok()?;
                    sshkey::inspect(&plain).map(|i| i.fingerprint)
                })?;
            Some(stored == wanted)
        };
        if !same_record_key && same_material() != Some(true) {
            continue;
        }
        let other_host = normalize_host(ip.as_deref().unwrap_or_default());
        if may_be_same_machine(
            &host,
            &other_host,
            &here,
            &host_keys(other_host_key.as_deref()),
        ) {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NEW: &str = "ssh-ed25519 AAAAnew cpk:0123456789ab:new-uuid";
    const OLD: &str = "ssh-ed25519 AAAAold";

    #[test]
    fn add_writes_the_new_key_next_to_every_line_of_the_old_key() {
        let commands = add_commands(NEW, OLD);
        assert_eq!(commands[0], r#"f="$HOME/.ssh/authorized_keys""#);
        assert_eq!(commands[1], r#"tmp=$(mktemp "$f.cpk-XXXXXX")"#);
        assert_eq!(commands[2], "old='ssh-ed25519 AAAAold'");
        assert_eq!(commands[3], "type=${old%% *}; key=${old#* }");
        assert!(
            commands[4].starts_with(
                r#"awk -v type="$type" -v key="$key" -v replacement='ssh-ed25519 AAAAnew cpk:0123456789ab:new-uuid' 'function token(    start,c,quoted,escaped) {"#
            ),
            "{}",
            commands[4]
        );
        assert!(
            commands[4].ends_with(
                r#"END { if (!found) exit 1 }' "$f" > "$tmp" || { echo "The old key is not in ~/.ssh/authorized_keys of the SSH user." >&2; exit 3; }"#
            ),
            "{}",
            commands[4]
        );
        assert_eq!(
            commands[5],
            r#"if [ -s "$f" ] && [ -n "$(tail -c1 "$f")" ]; then echo >> "$f"; fi"#
        );
        assert_eq!(
            commands[6],
            r#"while IFS= read -r entry; do grep -qxF -- "$entry" "$f" || printf '%s\n' "$entry" >> "$f"; done < "$tmp""#
        );
        assert_eq!(commands.len(), 7);
    }

    #[test]
    fn undo_removes_only_the_lines_that_add_wrote() {
        let commands = undo_add_commands(NEW, OLD);
        assert_eq!(commands[1], r#"if [ ! -f "$f" ]; then exit 0; fi"#);
        assert_eq!(commands[3], r#"cp -p "$f" "$tmp""#);
        assert!(
            commands[6].ends_with(r#""$f" > "$tmp.lines" || exit 0"#),
            "{}",
            commands[6]
        );
        assert_eq!(
            &commands[7..],
            [
                "rc=0",
                r#"grep -vxF -f "$tmp.lines" -- "$f" > "$tmp" || rc=$?"#,
                r#"if [ "$rc" -gt 1 ]; then exit 1; fi"#,
                r#"mv -f "$tmp" "$f""#,
            ]
        );
    }

    #[test]
    fn remove_deletes_every_line_with_the_key() {
        let commands = remove_commands("ssh-rsa AAAA'x");
        assert_eq!(
            commands[5],
            r#"grep -vF -- 'ssh-rsa AAAA'\''x' "$f" > "$tmp" || rc=$?"#
        );
        assert_eq!(
            commands.last().map(String::as_str),
            Some(r#"mv -f "$tmp" "$f""#)
        );
    }

    #[test]
    fn edits_run_under_the_lock_of_the_ssh_home() {
        let commands = locked_commands(vec!["true".to_owned()]);
        assert_eq!(
            commands[0],
            r#"if [ ! -d "$HOME/.ssh" ]; then mkdir -p "$HOME/.ssh" && chmod 700 "$HOME/.ssh"; fi"#
        );
        assert_eq!(
            commands[1],
            r#"lock="$HOME/.ssh/.coolify-authorized-keys.lock""#
        );
        assert_eq!(
            commands[3],
            r#"find "$lock" -maxdepth 0 -type d -mmin +10 -exec rmdir {} \; 2>/dev/null || true"#
        );
        assert_eq!(
            commands[10],
            r#"trap '[ -z "$tmp" ] || rm -f "$tmp" "$tmp.lines"; rmdir "$lock"' EXIT"#
        );
        assert_eq!(commands[11], "trap 'exit 1' HUP INT TERM");
        assert_eq!(commands[12], "true");
    }

    #[test]
    fn another_record_of_the_same_machine_keeps_the_key() {
        let key_a = vec!["ssh-ed25519 AAAA".to_owned()];
        let key_b = vec!["ssh-ed25519 BBBB".to_owned()];
        // The same address.
        assert!(may_be_same_machine(
            "203.0.113.7",
            "203.0.113.7",
            &key_a,
            &key_b
        ));
        // Different addresses, but the same host key: one machine.
        assert!(may_be_same_machine(
            "203.0.113.7",
            "100.64.0.1",
            &key_a,
            &key_a
        ));
        // Without a pinned host key on both records it is not known.
        assert!(may_be_same_machine(
            "203.0.113.7",
            "203.0.113.8",
            &[],
            &key_b
        ));
        // Different addresses and different host keys: different machines.
        assert!(!may_be_same_machine(
            "203.0.113.7",
            "203.0.113.8",
            &key_a,
            &key_b
        ));
        // A hostname cannot prove it.
        assert!(may_be_same_machine(
            "203.0.113.7",
            "web.example.com",
            &key_a,
            &key_b
        ));
    }

    #[test]
    fn hosts_and_host_keys_are_normalized() {
        assert_eq!(normalize_host(" Web.Example.com. "), "web.example.com");
        assert_eq!(normalize_host("2001:0DB8::0001"), "2001:db8::1");
        assert_eq!(
            host_keys(Some("ssh-ed25519 AAAA comment\n\nssh-rsa BBBB\n")),
            ["ssh-ed25519 AAAA", "ssh-rsa BBBB"]
        );
        assert!(host_keys(None).is_empty());
    }
}
