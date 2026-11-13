//! The terminal websocket (`docker/coolify-terminal`) and `TerminalSessionService`: who may connect,
//! which tokens open a session, and that the command comes from the server, never the browser.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::os::unix::fs::PermissionsExt as _;
use std::time::Duration;

use common::Harness;
use coolify_remote::{Remote, RemoteConfig};
use futures_util::{SinkExt as _, StreamExt as _};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;

const W: &str = "acme_owner_root";
const SHELL: &str = r#"PATH=$PATH:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin && if [ -f ~/.profile ]; then . ~/.profile; fi && if [ -n "$SHELL" ] && [ -x "$SHELL" ] && [ "${SHELL##*/}" != nologin ] && [ "${SHELL##*/}" != false ]; then exec $SHELL; elif command -v bash >/dev/null 2>&1; then exec bash; else sh; fi"#;

/// With a terminal on stdin it behaves as a remote shell that echoes lines; otherwise it answers
/// the checks of `sendTerminalCommand()`.
const FAKE_SSH: &str = r#"#!/bin/sh
for last; do :; done
if [ -t 0 ]; then
  printf '%s\n' "$@" > "$LOG_DIR/tty-args"
  printf 'term=%s\n' "$TERM" > "$LOG_DIR/tty-env"
  echo "READY"
  while read -r line; do
    case "$line" in
      size) stty size ;;
      exit) exit 0 ;;
      *) echo "GOT:$line" ;;
    esac
  done
  exit 0
fi
script=$(cat)
case "$script" in
  *"stopped-box"*) echo '{"State":{"Status":"exited"}}' ;;
  *"no-shell-box"*"bash -c"*) exit 1 ;;
  *"OpenStdin"*"console-box"*) echo true ;;
  *"OpenStdin"*) echo false ;;
  *"--format '{{json .}}'"*) echo '{"State":{"Status":"running"}}' ;;
  *) : ;;
esac
"#;

struct Fixture {
    _dir: tempfile::TempDir,
    log: std::path::PathBuf,
    h: Harness,
    addr: std::net::SocketAddr,
    server: String,
    server_id: i64,
    alice: (String, String),
}

async fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let log = root.join("log");
    std::fs::create_dir_all(&log).unwrap();
    let path = root.join("fake-ssh");
    let script = FAKE_SSH.replace("$LOG_DIR", log.to_str().unwrap());
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut cfg = RemoteConfig::new(root.join("data"));
    cfg.ssh_binary = path;
    let h = Harness::with_remote(Remote::new(cfg).unwrap()).await;

    let pem = coolify_compat::sshkey::generate(coolify_compat::sshkey::KeyKind::Ed25519).unwrap();
    let r = h
        .call(
            "POST",
            "/api/v1/security/keys",
            Some(W),
            Some(json!({"name": "key", "private_key": pem})),
        )
        .await;
    assert_eq!(r.status, 201, "{}", r.body);
    let key = r.json()["uuid"].as_str().unwrap().to_string();
    let r = h
        .call(
            "POST",
            "/api/v1/servers",
            Some(W),
            Some(json!({"name": "box", "ip": "203.0.113.30", "private_key_uuid": key, "user": "root"})),
        )
        .await;
    assert_eq!(r.status, 201, "{}", r.body);
    let server = r.json()["uuid"].as_str().unwrap().to_string();
    let server_id: i64 = sqlx::query_scalar("SELECT id FROM servers WHERE uuid = $1")
        .bind(&server)
        .fetch_one(h.tdb.db.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE server_settings SET is_terminal_enabled = true, is_reachable = true, is_usable = true WHERE server_id = $1")
        .bind(server_id)
        .execute(h.tdb.db.pool())
        .await
        .unwrap();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = h.app.clone();
    tokio::spawn(async move {
        axum::serve(listener, app.into_make_service())
            .await
            .unwrap();
    });
    let alice = h.ui_login("alice@example.test").await;
    Fixture {
        _dir: dir,
        log,
        h,
        addr,
        server,
        server_id,
        alice,
    }
}

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn open(f: &Fixture, cookie: Option<&str>, origin: Option<&str>) -> Socket {
    let mut req = format!("ws://{}/terminal/ws", f.addr)
        .into_client_request()
        .unwrap();
    if let Some(cookie) = cookie {
        req.headers_mut().insert("cookie", cookie.parse().unwrap());
    }
    if let Some(origin) = origin {
        req.headers_mut().insert("origin", origin.parse().unwrap());
    }
    tokio_tungstenite::connect_async(req).await.unwrap().0
}

/// The next frame as text (PTY output comes in binary frames, control messages in text frames),
/// or how the socket closed as `close:<code>:<reason>`.
async fn next(ws: &mut Socket) -> String {
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(15), ws.next())
            .await
            .expect("a frame in time");
        match frame {
            Some(Ok(Message::Text(t))) => return t.to_string(),
            Some(Ok(Message::Binary(b))) => return String::from_utf8_lossy(&b).into_owned(),
            Some(Ok(Message::Close(Some(c)))) => {
                return format!("close:{}:{}", u16::from(c.code), c.reason);
            }
            Some(Ok(Message::Close(None))) | None | Some(Err(_)) => return "closed".to_owned(),
            Some(Ok(_)) => {}
        }
    }
}

async fn send(ws: &mut Socket, message: Value) {
    ws.send(Message::text(message.to_string())).await.unwrap();
}

/// Reads until a frame contains `needle`, returning everything read.
async fn until(ws: &mut Socket, needle: &str) -> String {
    let mut seen = String::new();
    while !seen.contains(needle) {
        let frame = next(ws).await;
        assert!(
            frame != "closed" && !frame.starts_with("close:"),
            "{needle} not seen in {seen}; {frame}"
        );
        seen.push_str(&frame);
    }
    seen
}

async fn token(f: &Fixture, session: &(String, String), container: Option<&str>) -> Value {
    let r =
        f.h.request(
            "POST",
            "/ui/terminal/session",
            &[("cookie", &session.0), ("x-csrf-token", &session.1)],
            Some(json!({
                "is_container": container.is_some(),
                "identifier": container.unwrap_or("box"),
                "server_uuid": f.server,
            })),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    r.json()
}

const NOT_ALLOWED: &str = "You are not allowed to open a terminal for this resource.";

#[tokio::test]
async fn sockets_without_a_session_are_refused() {
    let f = fixture().await;
    let mut ws = open(&f, None, None).await;
    assert_eq!(
        next(&mut ws).await,
        "close:4401:Unauthorized: Missing required tokens"
    );
    let forged = format!(
        "{}={}",
        f.alice.0.split('=').next().unwrap(),
        "a".repeat(64)
    );
    let mut ws = open(&f, Some(&forged), None).await;
    assert_eq!(
        next(&mut ws).await,
        "close:4401:Unauthorized: Invalid credentials"
    );
    // `can.access.terminal`: a member of the team is not let in.
    let bob = f.h.ui_login("bob@example.test").await;
    let mut ws = open(&f, Some(&bob.0), None).await;
    assert_eq!(
        next(&mut ws).await,
        "close:4401:Unauthorized: Invalid credentials"
    );
    // A page of another site cannot use the cookie.
    let mut ws = open(&f, Some(&f.alice.0), Some("https://evil.test")).await;
    assert_eq!(
        next(&mut ws).await,
        "close:4401:Unauthorized: Invalid credentials"
    );
    // After signing out, the same cookie is refused.
    let carol = f.h.ui_login("carol@example.test").await;
    f.h.request(
        "DELETE",
        "/ui/session",
        &[("cookie", &carol.0), ("x-csrf-token", &carol.1)],
        None,
    )
    .await;
    let mut ws = open(&f, Some(&carol.0), None).await;
    assert_eq!(
        next(&mut ws).await,
        "close:4401:Unauthorized: Invalid credentials"
    );
    f.h.finish().await;
}

#[tokio::test]
async fn a_session_runs_the_command_of_the_server() {
    let f = fixture().await;
    let origin = format!("http://{}", f.addr);
    let mut ws = open(&f, Some(&f.alice.0), Some(&origin)).await;

    // Before a token nothing reaches a process, and a command from the browser means nothing.
    send(&mut ws, json!({"command": "touch /tmp/owned"})).await;
    send(&mut ws, json!({"message": "touch /tmp/owned\n"})).await;
    send(&mut ws, json!({"resize": {"cols": 10, "rows": 5}})).await;
    send(&mut ws, json!({"pause": true})).await;
    send(&mut ws, json!({"ping": true})).await;
    assert_eq!(next(&mut ws).await, "pong");
    // v4.7.5: `String(session.isActive)` in a text frame.
    send(&mut ws, json!({"checkActive": "check"})).await;
    assert_eq!(next(&mut ws).await, "false");

    let answer = token(&f, &f.alice, None).await;
    assert_eq!(answer["ok"], true, "{answer}");
    let t = answer["token"].as_str().unwrap().to_owned();
    assert_eq!(t.len(), 64);
    send(&mut ws, json!({"terminalToken": t, "command": "ignored"})).await;
    assert_eq!(next(&mut ws).await, "pty-ready");
    until(&mut ws, "READY").await;

    send(&mut ws, json!({"checkActive": "check"})).await;
    until(&mut ws, "true").await;
    send(&mut ws, json!({"message": "hello\r"})).await;
    until(&mut ws, "GOT:hello").await;
    send(&mut ws, json!({"resize": {"cols": 101, "rows": 37}})).await;
    send(&mut ws, json!({"message": "size\r"})).await;
    until(&mut ws, "37 101").await;
    // Output waits while paused and arrives on resume.
    send(&mut ws, json!({"pause": true})).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    send(&mut ws, json!({"message": "later\r"})).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(400), next(&mut ws))
            .await
            .is_err()
    );
    send(&mut ws, json!({"resume": true})).await;
    until(&mut ws, "GOT:later").await;
    send(&mut ws, json!({"message": "exit\r"})).await;
    until(&mut ws, "pty-exited").await;
    send(&mut ws, json!({"checkActive": "force"})).await;
    assert_eq!(next(&mut ws).await, "false");
    // `terminal.session.started` and `terminal.session.ended` (reason `exited`) are recorded.
    let mut recorded: Vec<String> = Vec::new();
    for _ in 0..50 {
        recorded = sqlx::query_scalar(
            "SELECT event::text FROM audit_events WHERE event LIKE 'terminal.session.%' ORDER BY id",
        )
        .fetch_all(f.h.tdb.db.pool())
        .await
        .unwrap();
        if recorded.len() >= 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(
        recorded,
        ["terminal.session.started", "terminal.session.ended"]
    );

    // The process got exactly what the service builds, in a TTY.
    let args = std::fs::read_to_string(f.log.join("tty-args")).unwrap();
    let lines: Vec<&str> = args.lines().collect();
    assert_eq!(lines.last().copied(), Some(SHELL));
    assert!(lines.contains(&"RequestTTY=yes"), "{args}");
    assert!(lines.contains(&"203.0.113.30"), "{args}");
    assert!(!args.contains("owned"), "{args}");
    assert_eq!(
        std::fs::read_to_string(f.log.join("tty-env"))
            .unwrap()
            .trim(),
        "term=xterm-color"
    );

    // Single use.
    // A rejected token is told by the close code alone, without a text message first.
    send(&mut ws, json!({"terminalToken": t})).await;
    assert_eq!(
        next(&mut ws).await,
        "close:4403:Unauthorized: Terminal token was rejected"
    );
    f.h.finish().await;
}

/// With an instance domain, Coolify's proxy configuration sends `/terminal/ws` to `coolify:6002`
/// and the rest to the main port. The terminal port serves the same session and nothing else.
#[tokio::test]
async fn the_terminal_port_serves_the_terminal_and_nothing_else() {
    let f = fixture().await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap();
    let router = coolify_api::terminal_ws::router(f.h.state.clone());
    tokio::spawn(async move {
        axum::serve(listener, router.into_make_service())
            .await
            .unwrap();
    });

    let mut req = format!("ws://{port}/terminal/ws")
        .into_client_request()
        .unwrap();
    req.headers_mut()
        .insert("cookie", f.alice.0.parse().unwrap());
    req.headers_mut()
        .insert("origin", format!("http://{port}").parse().unwrap());
    let mut ws = tokio_tungstenite::connect_async(req).await.unwrap().0;
    send(&mut ws, json!({"ping": true})).await;
    assert_eq!(next(&mut ws).await, "pong");
    let answer = token(&f, &f.alice, None).await;
    let t = answer["token"].as_str().unwrap().to_owned();
    send(&mut ws, json!({"terminalToken": t})).await;
    assert_eq!(next(&mut ws).await, "pty-ready");
    until(&mut ws, "READY").await;
    send(&mut ws, json!({"message": "hello\r"})).await;
    until(&mut ws, "GOT:hello").await;
    send(&mut ws, json!({"message": "exit\r"})).await;
    until(&mut ws, "pty-exited").await;

    for path in ["/", "/api/health", "/login", "/ui/terminal/session"] {
        let mut stream = tokio::net::TcpStream::connect(port).await.unwrap();
        let request = format!("GET {path} HTTP/1.0\r\nHost: {port}\r\n\r\n");
        tokio::io::AsyncWriteExt::write_all(&mut stream, request.as_bytes())
            .await
            .unwrap();
        let mut response = Vec::new();
        tokio::io::AsyncReadExt::read_to_end(&mut stream, &mut response)
            .await
            .unwrap();
        let response = String::from_utf8_lossy(&response);
        assert!(response.starts_with("HTTP/1.0 404"), "{path}: {response}");
    }
    f.h.finish().await;
}

#[tokio::test]
async fn tokens_belong_to_the_user_and_team_that_got_them() {
    let f = fixture().await;
    let refused = "close:4403:Unauthorized: Terminal token was rejected";

    // Malformed.
    let mut ws = open(&f, Some(&f.alice.0), None).await;
    send(&mut ws, json!({"terminalToken": "docker exec -it x sh"})).await;
    assert_eq!(
        next(&mut ws).await,
        "close:4403:Unauthorized: Invalid terminal token"
    );

    // Unknown.
    let mut ws = open(&f, Some(&f.alice.0), None).await;
    send(&mut ws, json!({"terminalToken": "a".repeat(64)})).await;
    assert_eq!(next(&mut ws).await, refused);

    // Another team's owner cannot redeem a token of Alice.
    let answer = token(&f, &f.alice, None).await;
    let carol = f.h.ui_login("carol@example.test").await;
    let mut ws = open(&f, Some(&carol.0), None).await;
    send(&mut ws, json!({"terminalToken": answer["token"]})).await;
    assert_eq!(next(&mut ws).await, refused);
    // That attempt used it up, as `Cache::pull` does.
    let mut ws = open(&f, Some(&f.alice.0), None).await;
    send(&mut ws, json!({"terminalToken": answer["token"]})).await;
    assert_eq!(next(&mut ws).await, refused);

    // A second session of the same user is a different session but the same user: allowed.
    let answer = token(&f, &f.alice, None).await;
    let other_login = f.h.ui_login("alice@example.test").await;
    let mut ws = open(&f, Some(&other_login.0), None).await;
    send(&mut ws, json!({"terminalToken": answer["token"]})).await;
    assert_eq!(next(&mut ws).await, "pty-ready");
    send(&mut ws, json!({"message": "exit\r"})).await;
    until(&mut ws, "pty-exited").await;

    // The server changed between issue and redemption.
    let answer = token(&f, &f.alice, None).await;
    sqlx::query("UPDATE server_settings SET is_terminal_enabled = false WHERE server_id = $1")
        .bind(f.server_id)
        .execute(f.h.tdb.db.pool())
        .await
        .unwrap();
    let mut ws = open(&f, Some(&f.alice.0), None).await;
    send(&mut ws, json!({"terminalToken": answer["token"]})).await;
    assert_eq!(next(&mut ws).await, refused);
    sqlx::query("UPDATE server_settings SET is_terminal_enabled = true WHERE server_id = $1")
        .bind(f.server_id)
        .execute(f.h.tdb.db.pool())
        .await
        .unwrap();

    // The private key has to be of the team.
    let answer = token(&f, &f.alice, None).await;
    let key: i64 = sqlx::query_scalar("SELECT private_key_id FROM servers WHERE id = $1")
        .bind(f.server_id)
        .fetch_one(f.h.tdb.db.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE private_keys SET team_id = 2 WHERE id = $1")
        .bind(key)
        .execute(f.h.tdb.db.pool())
        .await
        .unwrap();
    let mut ws = open(&f, Some(&f.alice.0), None).await;
    send(&mut ws, json!({"terminalToken": answer["token"]})).await;
    assert_eq!(next(&mut ws).await, refused);
    f.h.finish().await;
}

#[tokio::test]
async fn tokens_are_issued_as_upstream_decides() {
    let f = fixture().await;
    let bob = f.h.ui_login("bob@example.test").await;
    let carol = f.h.ui_login("carol@example.test").await;

    // Member and other team: the same answer, so another team's servers stay hidden.
    for who in [&bob, &carol] {
        let a = token(&f, who, None).await;
        assert_eq!(a, json!({"ok": false, "message": NOT_ALLOWED}));
    }
    // Page requests: `can.access.terminal`, then `firstOrFail()`.
    let path = format!("/ui/resources/server/{}/terminal", f.server);
    let r = f.h.request("GET", &path, &[("cookie", &bob.0)], None).await;
    assert_eq!(r.status, 403, "{}", r.body);
    let r =
        f.h.request("GET", &path, &[("cookie", &carol.0)], None)
            .await;
    assert_eq!(r.status, 404, "{}", r.body);
    let connect = format!("/ui/resources/server/{}/terminal/connect", f.server);
    for who in [&bob, &carol] {
        let r =
            f.h.request(
                "POST",
                &connect,
                &[("cookie", &who.0), ("x-csrf-token", &who.1)],
                Some(json!({})),
            )
            .await;
        assert_eq!(r.json(), json!({"ok": false, "message": NOT_ALLOWED}));
    }
    // CSRF on the request that issues a token.
    let r =
        f.h.request(
            "POST",
            "/ui/terminal/session",
            &[("cookie", &f.alice.0)],
            Some(json!({"server_uuid": f.server})),
        )
        .await;
    assert_eq!(r.status, 419, "{}", r.body);
    // No session.
    let r =
        f.h.request(
            "POST",
            "/ui/terminal/session",
            &[],
            Some(json!({"server_uuid": f.server})),
        )
        .await;
    assert_eq!(r.status, 401, "{}", r.body);

    // Container names cannot carry a command.
    for bad in ["a;touch /tmp/x", "$(id)", "-it", "a b", "../x", ""] {
        let a = token(&f, &f.alice, Some(bad)).await;
        assert_eq!(
            a,
            json!({"ok": false, "message": "The container name is not valid."}),
            "{bad}"
        );
    }
    assert_eq!(
        token(&f, &f.alice, Some("stopped-box")).await["message"],
        "The container is not running."
    );
    let a = token(&f, &f.alice, Some("no-shell-box")).await;
    assert_eq!(a["message"], "No shell is available in this container.");
    assert_eq!(a["has_shell"], false);

    // A running container, in Shell mode and, when it keeps stdin open, Console mode.
    let a = token(&f, &f.alice, Some("web-box")).await;
    assert_eq!(a["ok"], true, "{a}");
    assert_eq!(a["mode"], "shell");
    assert_eq!(a["attach_available"], false);
    let mut ws = open(&f, Some(&f.alice.0), None).await;
    send(&mut ws, json!({"terminalToken": a["token"]})).await;
    assert_eq!(next(&mut ws).await, "pty-ready");
    until(&mut ws, "READY").await;
    let args = std::fs::read_to_string(f.log.join("tty-args")).unwrap();
    assert_eq!(
        args.lines().last(),
        Some(format!("docker exec -it 'web-box' sh -c '{SHELL}'").as_str()),
    );
    // A second token on the socket replaces the running PTY.
    let a = token(&f, &f.alice, Some("console-box")).await;
    assert_eq!(a["mode"], "attach");
    assert_eq!(a["attach_available"], true);
    send(&mut ws, json!({"terminalToken": a["token"]})).await;
    let seen = until(&mut ws, "pty-ready").await;
    assert!(seen.contains("pty-exited"), "{seen}");
    until(&mut ws, "READY").await;
    let args = std::fs::read_to_string(f.log.join("tty-args")).unwrap();
    assert_eq!(
        args.lines().last(),
        Some(
            r#"docker logs --tail 200 'console-box' 2>&1; exec docker attach --detach-keys="ctrl-p,ctrl-q" --sig-proxy=false 'console-box'"#
        ),
    );

    // Disabled, or force disabled.
    let r =
        f.h.request(
            "POST",
            &connect,
            &[("cookie", &f.alice.0), ("x-csrf-token", &f.alice.1)],
            Some(json!({})),
        )
        .await;
    assert_eq!(r.json()["ok"], true, "{}", r.body);
    sqlx::query("UPDATE server_settings SET is_terminal_enabled = false WHERE server_id = $1")
        .bind(f.server_id)
        .execute(f.h.tdb.db.pool())
        .await
        .unwrap();
    assert_eq!(
        token(&f, &f.alice, None).await["message"],
        "Terminal access is disabled on this server."
    );
    sqlx::query("UPDATE server_settings SET is_terminal_enabled = true, force_disabled = true WHERE server_id = $1")
        .bind(f.server_id)
        .execute(f.h.tdb.db.pool())
        .await
        .unwrap();
    let r =
        f.h.request(
            "POST",
            &connect,
            &[("cookie", &f.alice.0), ("x-csrf-token", &f.alice.1)],
            Some(json!({})),
        )
        .await;
    assert_eq!(
        r.json(),
        json!({"ok": false, "message": "Server is disabled."})
    );
    f.h.finish().await;
}
