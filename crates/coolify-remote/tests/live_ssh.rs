//! Against the real SSH test host (scripts/testing-host-up.sh). Skipped unless
//! `COOLIFY_RS_TEST_SSH=host:port` and `COOLIFY_RS_TEST_SSH_KEY=<private key path>` are set.

use coolify_remote::{
    HOST_KEY_CHANGED_MESSAGE, HostKeyCheck, PrivateKey, Remote, RemoteConfig, RunOptions, Target,
};

fn host() -> Option<(String, i64, String)> {
    let addr = std::env::var("COOLIFY_RS_TEST_SSH").ok()?;
    let key = std::fs::read_to_string(std::env::var("COOLIFY_RS_TEST_SSH_KEY").ok()?).ok()?;
    let (h, p) = addr.rsplit_once(':')?;
    Some((h.to_owned(), p.parse().ok()?, key))
}

fn target(h: &str, port: i64, key: &str, user: &str) -> Target {
    Target {
        server_uuid: "live".into(),
        host: h.into(),
        port,
        user: user.into(),
        key: PrivateKey::new("k1", key.into()),
        connection_timeout: Some(5),
        cloudflare_tunnel: false,
        force_disabled: false,
        host_key: HostKeyCheck::Off,
    }
}

fn remote() -> (tempfile::TempDir, Remote) {
    let dir = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
    let data = dir
        .path()
        .canonicalize()
        .unwrap_or_else(|_| unreachable!())
        .join("d");
    let r = Remote::new(RemoteConfig::new(data)).unwrap_or_else(|e| panic!("{e}"));
    (dir, r)
}

#[tokio::test]
async fn runs_as_root_and_through_real_sudo() {
    let Some((h, port, key)) = host() else { return };
    let (_d, r) = remote();
    let root = r
        .run(
            &target(&h, port, &key, "root"),
            &["id -u"],
            RunOptions::op("test"),
        )
        .await;
    assert_eq!(root, Ok(Some("0".into())));
    // `id -u` becomes `sudo id -u` for a non-root user.
    let deploy = r
        .run(
            &target(&h, port, &key, "deploy"),
            &["id -u"],
            RunOptions::op("test"),
        )
        .await;
    assert_eq!(deploy, Ok(Some("0".into())));
    let plain = r
        .run(
            &target(&h, port, &key, "deploy"),
            &["id -un"],
            RunOptions::op("test").no_sudo(),
        )
        .await;
    assert_eq!(plain, Ok(Some("deploy".into())));
}

#[tokio::test]
async fn scans_pins_and_checks_the_host_key() {
    let Some((h, port, key)) = host() else { return };
    let (_d, r) = remote();
    let mut t = target(&h, port, &key, "root");

    // `readHostKey()`: the scan logs in with nothing and still reads the key.
    let scan = r.scan_host_key(&t).await;
    let pinned = scan.key.unwrap_or_else(|| panic!("no key: {}", scan.error));
    assert!(
        pinned.contains("ssh-ed25519") || pinned.contains("ecdsa") || pinned.contains("ssh-rsa"),
        "{pinned}"
    );

    // The pinned key: both modes connect.
    for enforce in [false, true] {
        t.host_key = HostKeyCheck::Pinned {
            keys: pinned.clone(),
            enforce,
        };
        assert!(r.run(&t, &["true"], RunOptions::op("test")).await.is_ok());
    }

    // Another key for the same host.
    let other = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIFuGmoeGq/pojrsyP1pszcNVuZx9iFkCELtxrh31QJ68";
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    r.on_host_key_mismatch(tx);

    // Enforce mode: OpenSSH refuses, and the error is upstream's message without the file path.
    t.host_key = HostKeyCheck::Pinned {
        keys: other.to_owned(),
        enforce: true,
    };
    let err = r
        .run(&t, &["true"], RunOptions::op("test").attempts(1))
        .await
        .err()
        .unwrap_or_else(|| unreachable!());
    assert_eq!(err.message, HOST_KEY_CHANGED_MESSAGE);
    assert_eq!(err.code, Some(255));
    let (uuid, stderr) = rx.try_recv().unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(uuid, "live");
    assert!(
        stderr.contains("REMOTE HOST IDENTIFICATION HAS CHANGED"),
        "{stderr}"
    );

    // Warn mode: OpenSSH connects, and the mismatch is reported.
    t.host_key = HostKeyCheck::Pinned {
        keys: other.to_owned(),
        enforce: false,
    };
    assert!(r.run(&t, &["true"], RunOptions::op("test")).await.is_ok());
    assert_eq!(
        rx.try_recv().map(|(uuid, _)| uuid).ok().as_deref(),
        Some("live")
    );

    r.forget_host_key("live");
}

#[tokio::test]
async fn reports_ssh_errors_like_upstream() {
    let Some((h, port, key)) = host() else { return };
    let (_d, r) = remote();
    // A valid key the host does not know.
    let dir = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
    let path = dir.path().join("other");
    let made = std::process::Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-f"])
        .arg(&path)
        .status();
    assert!(made.is_ok_and(|s| s.success()));
    let other = &std::fs::read_to_string(&path).unwrap_or_default();
    let err = r
        .run(
            &target(&h, port, other, "root"),
            &["true"],
            RunOptions::op("test").attempts(1),
        )
        .await
        .err();
    let err = err.unwrap_or_else(|| unreachable!());
    assert_eq!(err.code, Some(255));
    assert!(
        err.message.contains("Permission denied (publickey)"),
        "{}",
        err.message
    );

    // An unreadable key: the message names it like upstream's key file, not by its temporary path.
    // The reason after the name is OpenSSH's and depends on its version and crypto library.
    let err = r
        .run(
            &target(&h, port, "garbage", "root"),
            &["true"],
            RunOptions::op("test").attempts(1),
        )
        .await
        .err();
    let msg = err.map(|e| e.message).unwrap_or_default();
    assert!(
        ["invalid format", "error in libcrypto"]
            .iter()
            .any(|why| msg.starts_with(&format!("Load key \"ssh_key@k1\": {why}"))),
        "{msg}"
    );

    let err = r
        .run(
            &target("127.0.0.1", 1, &key, "root"),
            &["true"],
            RunOptions::op("test").attempts(1),
        )
        .await
        .err();
    let err = err.unwrap_or_else(|| unreachable!());
    assert_eq!(err.code, Some(255));
    assert!(
        err.message.contains("Connection refused"),
        "{}",
        err.message
    );

    let err = r
        .run(
            &target(&h, port, &key, "root"),
            &["echo out; echo bad >&2; exit 7"],
            RunOptions::op("test"),
        )
        .await
        .err();
    assert_eq!(
        err.map(|e| (e.message, e.code)),
        Some(("bad".into(), Some(7)))
    );
}
