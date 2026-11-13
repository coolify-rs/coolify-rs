//! Executor behaviour against a stand-in `ssh` script that records what it was given.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use coolify_remote::{
    HOST_KEY_CHANGED_MESSAGE, HostKeyCheck, PrivateKey, Remote, RemoteConfig, RunOptions, Target,
};

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
        host_key: HostKeyCheck::Off,
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
        "StrictHostKeyChecking=no",
        "UserKnownHostsFile=/dev/null",
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
    // Without a pinned key there is no alias (`hostKeyOptions()`).
    assert!(
        !args.iter().any(|a| a.starts_with("HostKeyAlias=")),
        "{args:?}"
    );

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
fn prunes_pins_of_servers_that_are_gone() {
    let dir = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
    let data = dir
        .path()
        .canonicalize()
        .unwrap_or_else(|_| unreachable!())
        .join("d");
    let remote = Remote::new(RemoteConfig::new(&data)).unwrap_or_else(|e| panic!("{e}"));
    let pins = data.join("ssh/known_hosts");
    for name in [
        "kept",
        "gone1",
        "gone2",
        "known_hosts_kept_0123456789abcdef",
        "known_hosts_gone1_0123456789abcdef",
        "scan_gone1_a1b2c3d4",
    ] {
        std::fs::write(pins.join(name), "x").unwrap_or_else(|_| unreachable!());
    }
    let live = ["kept".to_owned()].into_iter().collect();
    assert_eq!(remote.retain_host_keys(&live), 3);
    assert!(pins.join("kept").exists() && !pins.join("gone1").exists());
    assert!(pins.join("known_hosts_kept_0123456789abcdef").exists());
    assert!(!pins.join("known_hosts_gone1_0123456789abcdef").exists());
    // A scan that is running keeps its file.
    assert!(pins.join("scan_gone1_a1b2c3d4").exists());

    remote.forget_host_key("kept");
    assert!(!pins.join("kept").exists());
    assert!(!pins.join("known_hosts_kept_0123456789abcdef").exists());
}

// `SshMultiplexingHelper::hostKeyOptions()` with a pinned key.
#[tokio::test]
async fn pinned_host_key_options_follow_the_mode() {
    let f = fixture("true");
    let pinned = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIFuGmoeGq/pojrsyP1pszcNVuZx9iFkCELtxrh31QJ68";
    for (n, enforce) in [(0, false), (1, true)] {
        let mut t = target("root");
        t.host_key = HostKeyCheck::Pinned {
            keys: pinned.to_owned(),
            enforce,
        };
        f.remote
            .run(&t, &["true"], RunOptions::op("test"))
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        let args: Vec<String> = read(&f.root, &format!("args{n}"))
            .lines()
            .map(str::to_owned)
            .collect();
        assert!(
            args.iter().any(|a| a == "HostKeyAlias=coolify-srv1"),
            "{args:?}"
        );
        let strict = format!(
            "StrictHostKeyChecking={}",
            if enforce { "yes" } else { "no" }
        );
        assert!(args.contains(&strict), "{args:?}");
        let file = args
            .iter()
            .find_map(|a| a.strip_prefix("UserKnownHostsFile="))
            .unwrap_or_default()
            .to_owned();
        let dir = f.root.join("data/ssh/known_hosts");
        assert!(
            file.starts_with(&format!("{}/known_hosts_srv1_", dir.display())),
            "{file}"
        );
        assert_eq!(
            std::fs::read_to_string(&file).unwrap_or_default(),
            format!("coolify-srv1 {pinned}\n")
        );
        assert!(
            args.iter().any(|a| a == "GlobalKnownHostsFile=/dev/null"),
            "{args:?}"
        );
    }
}

// `excludeCertainErrors()` and `noticeHostKeyMismatch()`.
#[tokio::test]
async fn a_refused_host_key_is_reported_and_not_retried() {
    let f = fixture(
        "echo 'Host key for coolify-srv1 has changed and you have requested strict checking.' >&2; echo 'Host key verification failed.' >&2; exit 255",
    );
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    f.remote.on_host_key_mismatch(tx);
    let err = f
        .remote
        .run(&target("root"), &["true"], RunOptions::op("test"))
        .await
        .err();
    assert_eq!(
        err.as_ref().map(|e| (e.message.as_str(), e.code)),
        Some((HOST_KEY_CHANGED_MESSAGE, Some(255)))
    );
    // Upstream removed "Host key verification failed" from the retryable errors.
    assert_eq!(calls(&f.root), 1);
    assert_eq!(
        rx.try_recv().map(|(uuid, _)| uuid).ok().as_deref(),
        Some("srv1")
    );
}

// A program on the server (git) prints the same text for its own connection.
#[tokio::test]
async fn a_host_key_error_of_another_host_is_not_reported() {
    let f = fixture("echo 'Host key verification failed.' >&2; exit 128");
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    f.remote.on_host_key_mismatch(tx);
    let err = f
        .remote
        .run(&target("root"), &["git fetch"], RunOptions::op("test"))
        .await
        .err();
    assert_eq!(
        err.as_ref().map(|e| (e.message.as_str(), e.code)),
        Some(("Host key verification failed.", Some(128)))
    );
    assert!(rx.try_recv().is_err());
}

// `readHostKey()`: the options of the scan, and the key ssh saved in the scan file.
#[tokio::test]
async fn scans_the_host_key_without_any_login_method() {
    let f = fixture(
        r#"for a in $(cat "$LOG/args0"); do case "$a" in UserKnownHostsFile=*) echo 'coolify-srv1 ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIFuGmoeGq/pojrsyP1pszcNVuZx9iFkCELtxrh31QJ68' > "${a#UserKnownHostsFile=}";; esac; done; echo 'root@203.0.113.7: Permission denied (publickey).' >&2; exit 255"#,
    );
    let scan = f.remote.scan_host_key(&target("root")).await;
    assert_eq!(
        scan.key.as_deref(),
        Some("ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIFuGmoeGq/pojrsyP1pszcNVuZx9iFkCELtxrh31QJ68")
    );
    assert_eq!(
        scan.error,
        "root@203.0.113.7: Permission denied (publickey)."
    );
    let args: Vec<String> = read(&f.root, "args0").lines().map(str::to_owned).collect();
    for o in [
        "HostKeyAlias=coolify-srv1",
        "GlobalKnownHostsFile=/dev/null",
        "StrictHostKeyChecking=accept-new",
        "BatchMode=yes",
        "PubkeyAuthentication=no",
        "PasswordAuthentication=no",
        "KbdInteractiveAuthentication=no",
        "HostbasedAuthentication=no",
        "ConnectTimeout=7",
        "LogLevel=ERROR",
    ] {
        assert!(args.iter().any(|a| a == o), "missing {o}: {args:?}");
    }
    // The scan offers no key and runs no command of the caller.
    assert!(!args.iter().any(|a| a == "-i"), "{args:?}");
    assert_eq!(args.last().map(String::as_str), Some("true"));
    // The scan file is gone.
    let left: Vec<_> = std::fs::read_dir(f.root.join("data/ssh/known_hosts"))
        .map(|d| d.flatten().collect())
        .unwrap_or_default();
    assert!(left.is_empty(), "{left:?}");
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
