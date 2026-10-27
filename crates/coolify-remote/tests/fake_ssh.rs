//! Executor behaviour against a stand-in `ssh` script that records what it was given.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use coolify_remote::{PrivateKey, Remote, RemoteConfig, RunOptions, Target};

const KEY: &str =
    "-----BEGIN OPENSSH PRIVATE KEY-----\nnot-a-real-key\n-----END OPENSSH PRIVATE KEY-----";

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    remote: Remote,
}

/// `behaviour` is shell run after recording; `$LOG` is the log directory.
fn fixture(behaviour: &str) -> Fixture {
    let dir = tempfile::Builder::new()
        .prefix("remote")
        .tempdir()
        .unwrap_or_else(|_| unreachable!());
    // The data dir must pass the safe-character check.
    let root = dir.path().canonicalize().unwrap_or_else(|_| unreachable!());
    let log = root.join("log");
    std::fs::create_dir(&log).unwrap_or_else(|_| unreachable!());
    let script = format!(
        r#"#!/bin/sh
LOG={log}
n=$(ls "$LOG" | grep -c '^args' )
printf '%s\n' "$@" > "$LOG/args$n"
env > "$LOG/env$n"
while [ $# -gt 0 ]; do
  if [ "$1" = "-i" ]; then
    stat -c '%a' "$2" > "$LOG/mode$n" 2>/dev/null || stat -f '%Lp' "$2" > "$LOG/mode$n"
    cat "$2" > "$LOG/key$n"
    echo "$2" > "$LOG/keypath$n"
  fi
  shift
done
cat > "$LOG/stdin$n"
{behaviour}
"#,
        log = log.display()
    );
    let ssh = root.join("fake-ssh");
    std::fs::write(&ssh, script).unwrap_or_else(|_| unreachable!());
    std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o755))
        .unwrap_or_else(|_| unreachable!());
    let mut cfg = RemoteConfig::new(root.join("data"));
    cfg.ssh_binary = ssh;
    let remote = Remote::new(cfg).unwrap_or_else(|e| panic!("{e}"));
    Fixture {
        _dir: dir,
        root,
        remote,
    }
}

fn target(user: &str) -> Target {
    Target {
        server_uuid: "srv1".into(),
        host: "203.0.113.7".into(),
        port: 2222,
        user: user.into(),
        key: PrivateKey::new("k1", KEY.into()),
        connection_timeout: Some(7),
        cloudflare_tunnel: false,
        force_disabled: false,
    }
}

fn read(root: &Path, name: &str) -> String {
    std::fs::read_to_string(root.join("log").join(name)).unwrap_or_default()
}

fn calls(root: &Path) -> usize {
    std::fs::read_dir(root.join("log")).map_or(0, |d| {
        d.flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with("args"))
            .count()
    })
}

#[tokio::test]
async fn builds_a_hardened_argv_and_sends_the_script_on_stdin() {
    let f = fixture("echo '  hello  '");
    let out = f
        .remote
        .run(&target("root"), &["ls /", "uptime"], RunOptions::op("test"))
        .await;
    assert_eq!(out, Ok(Some("hello".into())));

    let args: Vec<String> = read(&f.root, "args0").lines().map(str::to_owned).collect();
    let tail: Vec<&str> = args
        .iter()
        .rev()
        .take(7)
        .rev()
        .map(String::as_str)
        .collect();
    assert_eq!(
        tail,
        [
            "-l",
            "root",
            "-p",
            "2222",
            "--",
            "203.0.113.7",
            r#"sh -c "if command -v bash >/dev/null 2>&1; then exec bash -se; else exec sh -se; fi""#
        ]
    );
    assert_eq!(&args[..2], ["-F", "/dev/null"]);
    for o in [
        "StrictHostKeyChecking=accept-new",
        "PasswordAuthentication=no",
        "BatchMode=yes",
        "IdentitiesOnly=yes",
        "ForwardAgent=no",
        "ClearAllForwardings=yes",
        "ConnectTimeout=7",
        "ServerAliveInterval=20",
        "RequestTTY=no",
        "LogLevel=ERROR",
    ] {
        assert!(args.iter().any(|a| a == o), "missing {o}: {args:?}");
    }
    let known = format!(
        "UserKnownHostsFile={}",
        f.root.join("data/ssh/known_hosts/srv1").display()
    );
    assert!(args.contains(&known), "{args:?}");

    assert_eq!(read(&f.root, "stdin0"), "ls /\nuptime\n");
    assert_eq!(read(&f.root, "mode0").trim(), "600");
    assert_eq!(read(&f.root, "key0"), format!("{KEY}\n"));
    // The key file is gone after the run.
    let key_path = read(&f.root, "keypath0");
    assert!(
        key_path
            .trim()
            .starts_with(&f.root.join("data/ssh/keys").display().to_string())
    );
    assert!(!Path::new(key_path.trim()).exists());
}

#[tokio::test]
async fn child_environment_is_cleared() {
    let f = fixture("true");
    f.remote
        .run(&target("root"), &["true"], RunOptions::op("test"))
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    let env = read(&f.root, "env0");
    assert!(!env.contains("CARGO"), "{env}");
    assert!(
        env.lines()
            .any(|l| l == format!("HOME={}", f.root.join("data/ssh/home").display())),
        "{env}"
    );
}

#[tokio::test]
async fn non_root_users_get_sudo_unless_disabled() {
    let f = fixture("true");
    let t = target("ubuntu");
    f.remote
        .run(
            &t,
            &["docker version", "command -v docker"],
            RunOptions::op("test"),
        )
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(
        read(&f.root, "stdin0"),
        "sudo docker version\ncommand -v docker\n"
    );
    f.remote
        .run(&t, &["docker version"], RunOptions::op("test").no_sudo())
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(read(&f.root, "stdin1"), "docker version\n");
}

#[tokio::test]
async fn errors_follow_upstream() {
    let f = fixture("echo '  boom  ' >&2; exit 3");
    let err = f
        .remote
        .run(&target("root"), &["x"], RunOptions::op("test"))
        .await
        .err();
    assert_eq!(
        err.as_ref().map(|e| (e.message.as_str(), e.code)),
        Some(("boom", Some(3)))
    );
    // A remote command's failure is not retried.
    assert_eq!(calls(&f.root), 1);

    let f = fixture("exit 2");
    let err = f
        .remote
        .run(&target("root"), &["x"], RunOptions::op("test"))
        .await
        .err();
    assert_eq!(
        err.map(|e| e.message),
        Some("SSH command failed with exit code: 2".into())
    );

    let f = fixture("echo null");
    assert_eq!(
        f.remote
            .run(&target("root"), &["x"], RunOptions::op("test"))
            .await,
        Ok(None)
    );
    assert_eq!(
        f.remote
            .run_quiet(&target("root"), &["x"], RunOptions::op("test"))
            .await,
        None
    );
}

#[tokio::test]
async fn connection_errors_are_retried() {
    let f = fixture(
        "echo 'ssh: connect to host 203.0.113.7 port 2222: Connection refused' >&2; exit 255",
    );
    let err = f
        .remote
        .run(&target("root"), &["x"], RunOptions::op("test").attempts(2))
        .await
        .err();
    assert_eq!(err.and_then(|e| e.code), Some(255));
    assert_eq!(calls(&f.root), 2);
}

#[tokio::test]
async fn deadline_kills_the_child_and_hides_the_command() {
    let f = fixture("sleep 30");
    let started = std::time::Instant::now();
    let err = f
        .remote
        .run(
            &target("root"),
            &["secret-command"],
            RunOptions::op("test").timeout(Duration::from_secs(1)),
        )
        .await
        .err();
    assert!(started.elapsed() < Duration::from_secs(10));
    let err = err.unwrap_or_else(|| unreachable!());
    assert_eq!(
        err.message,
        "The command on the server timed out after 1 seconds."
    );
    assert_eq!(err.code, Some(124));
}

#[tokio::test]
async fn refuses_option_injection_without_spawning() {
    let f = fixture("true");
    for (host, user, port) in [
        ("-oProxyCommand=touch /tmp/pwned", "root", 22),
        ("203.0.113.7", "-oProxyCommand=x", 22),
        ("203.0.113.7 -v", "root", 22),
        ("203.0.113.7", "ro ot", 22),
        ("203.0.113.7", "x;id", 22),
        ("203.0.113.7", "$(id)", 22),
        ("203.0.113.7", "root", 0),
        ("203.0.113.7", "root", 70000),
        ("a;b", "root", 22),
    ] {
        let mut t = target(user);
        t.host = host.into();
        t.port = port;
        assert!(
            f.remote
                .run(&t, &["x"], RunOptions::op("test"))
                .await
                .is_err(),
            "{host} {user} {port}"
        );
    }
    let mut t = target("root");
    t.server_uuid = "../escape".into();
    assert!(
        f.remote
            .run(&t, &["x"], RunOptions::op("test"))
            .await
            .is_err()
    );
    let mut t = target("root");
    t.force_disabled = true;
    assert_eq!(
        f.remote
            .run(&t, &["x"], RunOptions::op("test"))
            .await
            .err()
            .map(|e| e.message),
        Some("Server is disabled.".into())
    );
    assert_eq!(calls(&f.root), 0);
}

#[tokio::test]
async fn output_is_capped_and_sanitized() {
    let f = fixture("head -c 20000000 /dev/zero | tr '\\0' 'a'; printf '\\377'");
    let out = f
        .remote
        .run(&target("root"), &["x"], RunOptions::op("test"))
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(out.map(|o| o.len()), Some(16 * 1024 * 1024));
}

#[test]
fn rejects_unsafe_data_dirs_and_sweeps_old_keys() {
    assert!(Remote::new(RemoteConfig::new("/tmp/with space")).is_err());
    assert!(Remote::new(RemoteConfig::new("/tmp/%h")).is_err());

    let dir = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
    let data = dir
        .path()
        .canonicalize()
        .unwrap_or_else(|_| unreachable!())
        .join("d");
    std::fs::create_dir_all(data.join("ssh/keys")).unwrap_or_else(|_| unreachable!());
    std::fs::write(data.join("ssh/keys/key-leftover"), "x").unwrap_or_else(|_| unreachable!());
    Remote::new(RemoteConfig::new(&data)).unwrap_or_else(|e| panic!("{e}"));
    assert!(!data.join("ssh/keys/key-leftover").exists());
    let mode = std::fs::metadata(data.join("ssh/keys"))
        .map(|m| m.permissions().mode() & 0o777)
        .unwrap_or(0);
    assert_eq!(mode, 0o700);
}
