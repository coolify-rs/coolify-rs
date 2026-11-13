//! "SSH host keys" of the Git Source page (`Project\Application\Source`): the saved and published
//! host keys with their fingerprints, `removeHostKey()`, `fetchHostKeys()`,
//! `acceptFetchedHostKeys()`, `declineFetchedHostKeys()` and the known hosts checks of `submit()`.
//!
//! The known_hosts helpers (`gitKnownHostsLines()`, `gitKnownHostFingerprint()`,
//! `gitProviderHostKeys()`, `fetchGitProviderHostKeys()`, ...) are
//! `coolify_jobs::applications::git_ssh`; the hosts of the repository are `Jobs::git_ssh_host()`,
//! `Jobs::git_ssh_hosts_to_scan()` and `Jobs::fetch_git_ssh_host_keys()`.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use axum::Extension;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use coolify_db::applications::{self as apps, ApplicationRow};
use coolify_db::servers::ColumnValue;
use coolify_jobs::applications::git_ssh;
use serde_json::{Value, json};
use sha1::{Digest as _, Sha1};

use super::find;
use crate::auth::AuthCtx;
use crate::response::{ApiError, ApiResult, PhpJson, ok};
use crate::state::AppState;

/// The error of a known hosts text with a line that is no `host key-type key`.
pub(super) const INVALID_LINE: &str = "Each line must be \"host key-type key\" (for example \"[git.example.com]:2222 ssh-ed25519 AAAA...\"). Hashed hosts and comments are not supported.";

/// `$fetchedHostKeys` of the component: the keys that the Git host sent on the last "Fetch current
/// host key" of a user for an application, until they are accepted or declined. Kept in this
/// process for an hour.
type FetchedKeys = HashMap<(i64, i64), (Instant, Vec<String>)>;

static FETCHED: LazyLock<Mutex<FetchedKeys>> = LazyLock::new(|| Mutex::new(HashMap::new()));

const FETCHED_TTL: Duration = Duration::from_secs(3600);

fn set_fetched(user_id: i64, application_id: i64, lines: Vec<String>) {
    let mut map = FETCHED.lock().unwrap_or_else(|p| p.into_inner());
    map.retain(|_, (at, _)| at.elapsed() < FETCHED_TTL);
    if lines.is_empty() {
        map.remove(&(user_id, application_id));
    } else {
        map.insert((user_id, application_id), (Instant::now(), lines));
    }
}

fn take_fetched(user_id: i64, application_id: i64) -> Vec<String> {
    FETCHED
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .remove(&(user_id, application_id))
        .filter(|(at, _)| at.elapsed() < FETCHED_TTL)
        .map(|(_, lines)| lines)
        .unwrap_or_default()
}

/// PHP's `sha1($line)`.
fn sha1_hex(line: &str) -> String {
    Sha1::digest(line.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// `preg_split('/\R/', $text)` with `preg_replace('/\s+/', ' ', trim($line))` on every line.
fn normalized_lines(text: Option<&str>) -> Vec<String> {
    static NEWLINE: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"\r\n|[\n\x0B\x0C\r\u{85}\u{2028}\u{2029}]")
            .unwrap_or_else(|_| unreachable!())
    });
    NEWLINE
        .split(text.unwrap_or_default())
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect()
}

/// A row of the host key table: `gitKnownHostFingerprint($line)` with the id and whether the
/// provider publishes the key.
fn fingerprint_row(id: String, published: bool, line: &str, host: Option<&str>) -> Value {
    let f = git_ssh::git_known_host_fingerprint(line);
    json!({
        "id": id,
        "published": published,
        "host": host.unwrap_or(&f.host),
        "type": f.key_type,
        "fingerprint": f.fingerprint,
    })
}

/// `knownHostFingerprints()`: the SHA256 fingerprints of the saved host keys, in the format that
/// `ssh-keygen -lf` shows. The id identifies the line, because two hosts can have the same key.
fn known_host_fingerprints(saved: Option<&str>) -> Vec<Value> {
    git_ssh::git_known_hosts_lines(saved)
        .iter()
        .map(|line| fingerprint_row(sha1_hex(line), false, line, None))
        .collect()
}

/// `publishedHostKeyFingerprints()`: the keys that deployments use for a repository on GitHub,
/// GitLab.com, or Bitbucket Cloud (`gitProviderHostKeys()`). `host` is the SSH host of the
/// repository.
fn published_host_key_fingerprints(host: Option<&str>, provider_keys: &[String]) -> Vec<Value> {
    let host = host.unwrap_or_default().to_lowercase();
    if !git_ssh::git_pinned_hosts().contains(&host) {
        return Vec::new();
    }
    provider_keys
        .iter()
        .filter(|line| git_ssh::git_known_hosts_line_hosts(line).contains(&host))
        .map(|line| {
            fingerprint_row(
                format!("published-{}", sha1_hex(line)),
                true,
                line,
                Some(&host),
            )
        })
        .collect()
}

/// The values of the "SSH host keys" section, merged into the answer of `Source::mount()`:
/// whether the section shows (`$usesGitSsh || filled($application->git_ssh_known_hosts)`), the
/// saved text and the table rows (published keys first).
pub(super) async fn section(state: &AppState, a: &ApplicationRow) -> Value {
    let saved = a
        .str("git_ssh_known_hosts")
        .filter(|t| !t.trim().is_empty());
    let ssh_host = match state.jobs.as_ref() {
        Some(jobs) => jobs.git_ssh_host(a).await,
        None => None,
    };
    let mut rows = published_host_key_fingerprints(
        ssh_host.as_ref().map(|h| h.host.as_str()),
        &git_ssh::git_provider_host_keys(),
    );
    rows.extend(known_host_fingerprints(saved));
    json!({
        "uses_git_ssh": ssh_host.is_some(),
        "show_ssh_host_keys": ssh_host.is_some() || saved.is_some(),
        "git_ssh_known_hosts": a.str("git_ssh_known_hosts"),
        "host_key_fingerprints": rows,
    })
}

/// The known hosts part of `Source::submit()`: `Err` is the message of the validation error on
/// `gitSshKnownHosts`. `Ok(Some(value))` is the new `git_ssh_known_hosts` (the text or null) when
/// the user changed the keys; `Ok(None)` leaves the column, so keys that a deployment saved after
/// the page opened stay. `loaded` is the text the form was loaded with.
pub(super) fn submitted_known_hosts(
    text: Option<&str>,
    loaded: Option<&str>,
) -> Result<Option<Option<String>>, String> {
    let lines: Vec<String> = normalized_lines(text)
        .into_iter()
        .filter(|line| !line.is_empty())
        .collect();
    if lines
        .iter()
        .any(|line| !git_ssh::is_valid_git_known_hosts_line(line))
    {
        return Err(INVALID_LINE.into());
    }
    let loaded_lines = normalized_lines(loaded);
    if lines.iter().any(|line| {
        git_ssh::git_known_hosts_line_has_host_with_published_keys(line)
            && !loaded_lines.contains(line)
    }) {
        return Err(format!(
            "Coolify always uses the published host keys of {}. Remove the lines of these hosts.",
            git_ssh::git_hosts_with_published_keys().join(", ")
        ));
    }
    // `#[Validate(['nullable', 'string', 'max:65535'])]`, checked by `syncData()`.
    if text.is_some_and(|t| t.chars().count() > 65535) {
        return Err(
            "The git ssh known hosts field must not be greater than 65535 characters.".into(),
        );
    }
    let new = git_ssh::git_known_hosts_lines(text);
    if new == git_ssh::git_known_hosts_lines(loaded) {
        return Ok(None);
    }
    Ok(Some((!new.is_empty()).then(|| new.join("\n"))))
}

/// `implode("\n", $lines) ?: null` as the column value.
fn column(lines: &[String]) -> ColumnValue {
    ColumnValue::Text((!lines.is_empty()).then(|| lines.join("\n")))
}

async fn store(state: &AppState, a: &ApplicationRow, lines: &[String]) -> Result<(), ApiError> {
    apps::update_if_changed(
        &state.db,
        a.id,
        &[("git_ssh_known_hosts", column(lines))],
        state.now(),
    )
    .await?;
    Ok(())
}

fn failure(message: impl Into<String>) -> ApiResult {
    Ok(PhpJson(
        StatusCode::UNPROCESSABLE_ENTITY,
        json!({"message": message.into()}),
    )
    .into_response())
}

/// The saved lines without the line with this id (`sha1($line)`).
fn without_line(saved: Option<&str>, id: &str) -> Vec<String> {
    git_ssh::git_known_hosts_lines(saved)
        .into_iter()
        .filter(|line| sha1_hex(line) != id)
        .collect()
}

/// `Source::removeHostKey($id)`: removes the saved host key line with this id. The next
/// deployment saves the key that the host sends then.
pub async fn remove(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path((uuid, id)): Path<(String, String)>,
) -> ApiResult {
    let a = find(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let lines = without_line(a.str("git_ssh_known_hosts"), &id);
    store(&state, &a, &lines).await?;
    Ok(ok(json!({"message": "Host key removed."})).into_response())
}

/// `host (port N)` of the hosts that sent no key.
fn host_names(hosts: &[coolify_jobs::applications::GitSshHost]) -> String {
    hosts
        .iter()
        .map(|h| format!("{} (port {})", h.host, h.port))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The fetched lines that need an accept: a gitlab.com key that is the same as a built-in key
/// needs none (`array_diff($fetchedHostKeys, $builtInHostKeys)`).
fn lines_to_accept(fetched: &[String], provider_keys: &[String]) -> Vec<String> {
    let built_in: Vec<String> = provider_keys
        .iter()
        .flat_map(|line| {
            let (hosts, key) = line.split_once(' ').unwrap_or((line.as_str(), ""));
            hosts
                .split(',')
                .map(|host| format!("{host} {key}"))
                .collect::<Vec<_>>()
        })
        .collect();
    fetched
        .iter()
        .filter(|line| !built_in.contains(line))
        .cloned()
        .collect()
}

/// `Source::fetchHostKeys()`: reads the keys that the Git host sends now (and, for GitHub and
/// Bitbucket Cloud, the keys they publish). The user accepts or declines the fetched keys.
pub async fn fetch(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let a = find(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let jobs = state.jobs.as_ref().ok_or(ApiError::NotPorted)?;
    set_fetched(ctx.user_id, a.id, Vec::new());
    let ssh_host = jobs.git_ssh_host(&a).await;
    let hosts = jobs.git_ssh_hosts_to_scan(&a).await;
    if ssh_host.is_none() && hosts.is_empty() {
        return failure("The repository does not use SSH.");
    }
    let mut success: Vec<String> = Vec::new();
    let mut error: Option<String> = None;
    if let Some(host) = &ssh_host
        && let Some(provider_hosts) = git_ssh::git_provider_host_key_source(&host.host)
    {
        let count = match git_ssh::fetch_git_provider_host_keys(provider_hosts).await {
            Ok(lines) => lines.len(),
            Err(e) => return failure(e),
        };
        success.push(format!(
            "Deployments now use the {count} host keys that {} publishes.",
            host.host
        ));
    }
    let mut to_accept: Vec<String> = Vec::new();
    if !hosts.is_empty() {
        let fetched = jobs.fetch_git_ssh_host_keys(&a).await;
        to_accept = lines_to_accept(&fetched, &git_ssh::git_provider_host_keys());
        if fetched.is_empty() {
            error = Some(format!(
                "Could not read the host keys of {}. To add a key, click Edit manually and paste it.",
                host_names(&hosts)
            ));
        } else if to_accept.is_empty() {
            success.push("The host sends the built-in published keys. Nothing to accept.".into());
        }
    }
    // `fetchedHostKeyFingerprints()`.
    let fingerprints: Vec<Value> = to_accept
        .iter()
        .map(|line| {
            let f = git_ssh::git_known_host_fingerprint(line);
            json!({"host": f.host, "type": f.key_type, "fingerprint": f.fingerprint})
        })
        .collect();
    set_fetched(ctx.user_id, a.id, to_accept);
    Ok(ok(json!({
        "success": success,
        "error": error,
        "fetched_host_keys": fingerprints,
    }))
    .into_response())
}

/// `Source::acceptFetchedHostKeys()`: replaces the saved keys of the fetched host with the
/// fetched keys.
pub async fn accept(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let a = find(&state, &ctx, &uuid).await?;
    if !ctx.is_team_admin() {
        return Err(ApiError::Unauthorized);
    }
    let fetched = take_fetched(ctx.user_id, a.id);
    let lines = git_ssh::git_known_hosts_replace_host_keys(a.str("git_ssh_known_hosts"), &fetched);
    store(&state, &a, &lines).await?;
    Ok(
        ok(json!({"message": "Host keys saved. Later deployments accept only these keys."}))
            .into_response(),
    )
}

/// `Source::declineFetchedHostKeys()`.
pub async fn decline(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthCtx>,
    Path(uuid): Path<String>,
) -> ApiResult {
    let a = find(&state, &ctx, &uuid).await?;
    take_fetched(ctx.user_id, a.id);
    Ok(ok(json!({"ok": true})).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl";
    const OTHER: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAfuCHKVTjquxvt6CM6tdG4SLp1Btn/nOeHHE5UOzRdf";

    #[test]
    fn ids_are_the_sha1_of_the_line() {
        assert_eq!(sha1_hex("abc"), "a9993e364706816aba3e25717850c26c9cd0d89d");
    }

    #[test]
    fn saved_keys_are_listed_with_their_fingerprint() {
        let saved = format!("git.example.com {KEY}\n\n  [git.example.com]:2222   {OTHER}  \n");
        let rows = known_host_fingerprints(Some(&saved));
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["host"], "git.example.com");
        assert_eq!(rows[0]["type"], "ssh-ed25519");
        assert_eq!(rows[0]["published"], false);
        // The published GitHub ed25519 fingerprint.
        assert_eq!(
            rows[0]["fingerprint"],
            "SHA256:+DiY3wvvV6TuJJhbpZisF/zLDA0zPMSvHdkr4UvCOqU"
        );
        assert_eq!(rows[0]["id"], sha1_hex(&format!("git.example.com {KEY}")));
        assert_eq!(rows[1]["host"], "[git.example.com]:2222");
        assert!(known_host_fingerprints(None).is_empty());
    }

    #[test]
    fn published_keys_show_only_for_a_pinned_host() {
        let keys = git_ssh::git_provider_host_keys();
        let rows = published_host_key_fingerprints(Some("GitHub.com"), &keys);
        assert_eq!(rows.len(), 3);
        assert!(rows.iter().all(|r| r["published"] == true));
        assert!(rows.iter().all(|r| r["host"] == "github.com"));
        assert!(rows.iter().all(|r| {
            r["id"]
                .as_str()
                .is_some_and(|id| id.starts_with("published-"))
        }));
        assert!(published_host_key_fingerprints(Some("git.example.com"), &keys).is_empty());
        assert!(published_host_key_fingerprints(None, &keys).is_empty());
    }

    #[test]
    fn a_submitted_text_is_saved_only_when_the_user_changed_it() {
        let loaded = format!("git.example.com {KEY}");
        // Untouched (also with other spacing): the column is left alone.
        assert_eq!(
            submitted_known_hosts(Some(&loaded), Some(&loaded)),
            Ok(None)
        );
        let spaced = format!("  git.example.com   {KEY}\r\n");
        assert_eq!(
            submitted_known_hosts(Some(&spaced), Some(&loaded)),
            Ok(None)
        );
        assert_eq!(submitted_known_hosts(None, None), Ok(None));
        // A new line is added, lines are normalized and deduplicated.
        let edited =
            format!("git.example.com {KEY}\nother.example.com  {OTHER}\ngit.example.com {KEY}");
        assert_eq!(
            submitted_known_hosts(Some(&edited), Some(&loaded)),
            Ok(Some(Some(format!(
                "git.example.com {KEY}\nother.example.com {OTHER}"
            ))))
        );
        // Emptied: null.
        assert_eq!(
            submitted_known_hosts(Some(" \n"), Some(&loaded)),
            Ok(Some(None))
        );
    }

    #[test]
    fn a_submitted_text_must_hold_plain_known_hosts_lines() {
        for bad in [
            "git.example.com".to_owned(),
            format!("|1|abc=|def= {KEY}"),
            format!("git.example.com {KEY} a comment"),
            format!("# comment\ngit.example.com {KEY}"),
        ] {
            assert_eq!(
                submitted_known_hosts(Some(&bad), None),
                Err(INVALID_LINE.to_owned()),
                "{bad}"
            );
        }
    }

    #[test]
    fn hosts_with_published_keys_cannot_be_added() {
        let github = format!("github.com {KEY}");
        let error = submitted_known_hosts(Some(&github), None).unwrap_err();
        assert!(error.starts_with("Coolify always uses the published host keys of github.com, "));
        assert!(error.ends_with(". Remove the lines of these hosts."));
        // A line that was already there when the form loaded is not an error; it is dropped on save.
        assert_eq!(
            submitted_known_hosts(Some(&github), Some(&github)),
            Ok(None)
        );
        // gitlab.com publishes no keys over HTTPS, so its keys can be saved.
        let gitlab = format!("gitlab.com {OTHER}");
        assert_eq!(
            submitted_known_hosts(Some(&gitlab), None),
            Ok(Some(Some(gitlab.clone())))
        );
    }

    #[test]
    fn removing_a_key_keeps_the_other_lines() {
        let first = format!("git.example.com {KEY}");
        let second = format!("other.example.com {OTHER}");
        let saved = format!("{first}\n{second}");
        assert_eq!(
            without_line(Some(&saved), &sha1_hex(&first)),
            vec![second.clone()]
        );
        assert_eq!(without_line(Some(&saved), "unknown"), vec![first, second]);
        assert!(matches!(column(&[]), ColumnValue::Text(None)));
    }

    #[test]
    fn built_in_keys_need_no_accept() {
        let provider = vec![format!("gitlab.com,[altssh.gitlab.com]:443 {OTHER}")];
        let fetched = vec![
            format!("gitlab.com {OTHER}"),
            format!("gitlab.com {KEY}"),
            format!("git.example.com {KEY}"),
        ];
        assert_eq!(
            lines_to_accept(&fetched, &provider),
            vec![
                format!("gitlab.com {KEY}"),
                format!("git.example.com {KEY}")
            ]
        );
        assert!(lines_to_accept(&[], &provider).is_empty());
    }
}
