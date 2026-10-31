//! Upstream's `docker/coolify-terminal/terminal-server.js`, as a websocket of this server.
//!
//! The browser sends only JSON messages: keystrokes, a window size, flow control, a ping and a
//! single-use token. The token selects a session prepared on the server; a command never comes
//! from the browser, so upstream's parsing of an ssh command line has no counterpart here.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use axum::extract::State;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::http::{HeaderMap, header};
use axum::response::Response;
use coolify_remote::TtyInvocation;
use pty_process::{Pty, Size};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{mpsc, watch};
use tokio::time::Instant;

use crate::auth::AuthCtx;
use crate::state::AppState;
use crate::ui_terminal::{Session, redeem};

/// `TERMINAL_CLOSE_CODES`.
const AUTH_REJECTED: u16 = 4401;
const TOKEN_REJECTED: u16 = 4403;
const AUTH_UNAVAILABLE: u16 = 1011;
/// `HEARTBEAT_INTERVAL_MS`.
const HEARTBEAT: Duration = Duration::from_secs(30);
/// `MAX_TERMINAL_SESSION_TIMEOUT_SECONDS`.
const MAX_SESSION: Duration = Duration::from_secs(8 * 60 * 60);
/// The output a PTY may queue between its reader and the socket.
const OUTPUT_QUEUE: usize = 64;

static NEXT_PTY: AtomicU64 = AtomicU64::new(1);

/// `createTerminalUpgradeHandler`: a rejected client is still upgraded, because a browser cannot
/// read the status of a failed handshake and needs the close code.
pub async fn upgrade(
    State(state): State<AppState>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    ws.on_upgrade(move |socket| run(socket, state, headers))
}

enum Auth {
    Allowed,
    Rejected(u16, &'static str),
}

/// A page on another site can open a websocket with the session cookie, so its origin must be
/// this server. Upstream relies on `SameSite` and the XSRF cookie for this.
fn same_origin(headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(header::ORIGIN) else {
        return true;
    };
    let Some(origin) = origin
        .to_str()
        .ok()
        .and_then(|o| o.split_once("://"))
        .map(|(_, host)| host.to_ascii_lowercase())
    else {
        return false;
    };
    ["x-forwarded-host", header::HOST.as_str()]
        .iter()
        .filter_map(|name| headers.get(*name).and_then(|v| v.to_str().ok()))
        .any(|host| host.to_ascii_lowercase() == origin)
}

/// `authenticateTerminalUpgrade`, with `/terminal/auth` and `can.access.terminal`.
async fn authenticate(state: &AppState, headers: &HeaderMap) -> Auth {
    if !headers.contains_key(header::COOKIE) {
        return Auth::Rejected(AUTH_REJECTED, "Unauthorized: Missing required tokens");
    }
    if !same_origin(headers) {
        return Auth::Rejected(AUTH_REJECTED, "Unauthorized: Invalid credentials");
    }
    match crate::session::terminal_ctx(state, headers).await {
        Ok(ctx) if ctx.is_team_admin() => Auth::Allowed,
        Ok(_) | Err(crate::response::ApiError::Unauthenticated) => {
            Auth::Rejected(AUTH_REJECTED, "Unauthorized: Invalid credentials")
        }
        Err(crate::response::ApiError::Internal) => {
            Auth::Rejected(AUTH_UNAVAILABLE, "Internal Server Error")
        }
        Err(_) => Auth::Rejected(AUTH_REJECTED, "Unauthorized: Invalid credentials"),
    }
}

/// `TerminalSessionService::redeem()` behind `POST /terminal/session`, whose guard reads the
/// session again: the user may have signed out or changed team since the socket opened.
async fn redeem_for(state: &AppState, headers: &HeaderMap, token: &str) -> Result<Session, ()> {
    let ctx: AuthCtx = crate::session::terminal_ctx(state, headers)
        .await
        .map_err(|_| ())?;
    if !ctx.is_team_admin() {
        return Err(());
    }
    redeem(state, &ctx, token).await
}

enum Event {
    Output(u64, String),
    Exited(u64),
    Redeemed(Result<Box<Session>, ()>),
    /// A previous PTY was to be replaced by this session.
    Replaced(bool, Box<Session>),
}

enum Input {
    Bytes(Vec<u8>),
    Resize(u16, u16),
}

/// The handles of one PTY; its tasks end when the process does.
struct Running {
    id: u64,
    input: mpsc::UnboundedSender<Input>,
    paused: watch::Sender<bool>,
    exited: watch::Receiver<bool>,
    kill: mpsc::Sender<()>,
    attach: bool,
    deadline: Instant,
    expired: bool,
}

impl Running {
    fn stopper(&self) -> Stopper {
        Stopper {
            input: self.input.clone(),
            exited: self.exited.clone(),
            kill: self.kill.clone(),
            attach: self.attach,
        }
    }
}

struct Stopper {
    input: mpsc::UnboundedSender<Input>,
    exited: watch::Receiver<bool>,
    kill: mpsc::Sender<()>,
    attach: bool,
}

impl Stopper {
    async fn wait_exit(&mut self, limit: Duration) -> bool {
        let exited = &mut self.exited;
        tokio::time::timeout(limit, exited.wait_for(|e| *e))
            .await
            .is_ok_and(|r| r.is_ok())
            || *exited.borrow()
    }

    /// `killPtyProcess()`, or `detachPtyProcess()` for an attach session: it shares the main
    /// process of the container, so it is detached and never killed.
    async fn stop(mut self) -> bool {
        if self.attach {
            for _ in 0..3 {
                let _ = self.input.send(Input::Bytes(b"\x10\x11".to_vec()));
                if self.wait_exit(Duration::from_millis(500)).await {
                    return true;
                }
            }
            let _ = self.kill.send(()).await;
            return self.wait_exit(Duration::from_secs(1)).await;
        }
        for _ in 0..5 {
            // Ctrl-C first, so interactive programs quit instead of reading the command as keys.
            let _ = self.input.send(Input::Bytes(
                b"\x03set +o history\nkill -TERM -$$ && exit\nset -o history\n".to_vec(),
            ));
            if self.wait_exit(Duration::from_millis(500)).await {
                return true;
            }
        }
        // Ending the local ssh client makes sshd hang up the remote session.
        let _ = self.kill.send(()).await;
        false
    }
}

/// The text of a chunk of output, keeping a character split across two reads for the next one.
fn decode(pending: &mut Vec<u8>, chunk: &[u8]) -> String {
    pending.extend_from_slice(chunk);
    let keep = match std::str::from_utf8(pending) {
        Ok(_) => 0,
        Err(e) if e.error_len().is_none() => pending.len() - e.valid_up_to(),
        Err(_) => {
            let tail = pending.len().saturating_sub(3);
            pending[tail..]
                .iter()
                .rposition(|b| *b >= 0xC0)
                .map_or(0, |at| {
                    let at = tail + at;
                    match std::str::from_utf8(&pending[at..]) {
                        Err(e) if e.error_len().is_none() => pending.len() - at,
                        _ => 0,
                    }
                })
        }
    };
    let rest = pending.split_off(pending.len() - keep);
    let text = String::from_utf8_lossy(pending).into_owned();
    *pending = rest;
    text
}

/// `pty.spawn('ssh', args, { name: 'xterm-color', cols: 80, rows: 30, env })`.
fn spawn(
    invocation: TtyInvocation,
    attach: bool,
    events: mpsc::Sender<Event>,
) -> Result<Running, String> {
    let TtyInvocation {
        program,
        args,
        envs,
        key,
    } = invocation;
    let (pty, pts) = pty_process::open().map_err(|e| e.to_string())?;
    pty.resize(Size::new(30, 80)).map_err(|e| e.to_string())?;
    let child = pty_process::Command::new(program)
        .env_clear()
        .envs(envs)
        .env("TERM", "xterm-color")
        .args(args)
        .kill_on_drop(true)
        .spawn(pts)
        .map_err(|e| e.to_string())?;
    Ok(start_tasks(pty, child, key, attach, events))
}

fn start_tasks(
    pty: Pty,
    mut child: tokio::process::Child,
    key: Box<dyn std::any::Any + Send>,
    attach: bool,
    events: mpsc::Sender<Event>,
) -> Running {
    let id = NEXT_PTY.fetch_add(1, Ordering::Relaxed);
    let (mut read, mut write) = pty.into_split();
    let (input, mut input_rx) = mpsc::unbounded_channel::<Input>();
    let (paused, mut paused_rx) = watch::channel(false);
    let (exited_tx, exited) = watch::channel(false);
    let (kill, mut kill_rx) = mpsc::channel::<()>(1);

    tokio::spawn(async move {
        while let Some(message) = input_rx.recv().await {
            match message {
                Input::Bytes(bytes) => {
                    if write.write_all(&bytes).await.is_err() {
                        break;
                    }
                }
                Input::Resize(cols, rows) => {
                    let _ = write.resize(Size::new(rows, cols));
                }
            }
        }
    });

    let output = events.clone();
    let mut reader = tokio::spawn(async move {
        let mut buffer = [0u8; 8192];
        let mut pending = Vec::new();
        loop {
            // `ptyProcess.pause()`: output waits in the kernel until the browser has caught up.
            if paused_rx.wait_for(|p| !*p).await.is_err() {
                return;
            }
            let n = match read.read(&mut buffer).await {
                Ok(0) | Err(_) => return,
                Ok(n) => n,
            };
            let text = decode(&mut pending, &buffer[..n]);
            if !text.is_empty() && output.send(Event::Output(id, text)).await.is_err() {
                return;
            }
        }
    });

    tokio::spawn(async move {
        tokio::select! {
            _ = child.wait() => {}
            _ = kill_rx.recv() => {
                let _ = child.start_kill();
                let _ = child.wait().await;
            }
        }
        drop(key);
        // The last output comes before the exit; a paused reader or a descendant holding the
        // terminal open must not hold it back.
        if tokio::time::timeout(Duration::from_millis(250), &mut reader)
            .await
            .is_err()
        {
            reader.abort();
        }
        let _ = events.send(Event::Exited(id)).await;
        let _ = exited_tx.send(true);
    });

    Running {
        id,
        input,
        paused,
        exited,
        kill,
        attach,
        deadline: Instant::now() + MAX_SESSION,
        expired: false,
    }
}

fn text(message: impl Into<String>) -> Message {
    Message::Text(message.into().into())
}

/// `rejectTerminalSocket()`.
async fn reject(socket: &mut WebSocket, code: u16, reason: &'static str, message: Option<&str>) {
    if let Some(message) = message {
        let _ = socket.send(text(message)).await;
    }
    let _ = socket
        .send(Message::Close(Some(CloseFrame {
            code,
            reason: reason.into(),
        })))
        .await;
}

/// `handleTerminalToken()`: an alphanumeric token of 64 characters.
fn token_shape_ok(token: &Value) -> Option<&str> {
    token
        .as_str()
        .filter(|t| t.len() == 64 && t.bytes().all(|b| b.is_ascii_alphanumeric()))
}

/// One connection: `wss.on('connection')` with its handlers.
async fn run(mut socket: WebSocket, state: AppState, headers: HeaderMap) {
    if let Auth::Rejected(code, reason) = authenticate(&state, &headers).await {
        reject(&mut socket, code, reason, None).await;
        return;
    }
    let (events, mut events_rx) = mpsc::channel::<Event>(OUTPUT_QUEUE);
    let mut current: Option<Running> = None;
    let mut alive = true;
    let mut heartbeat = tokio::time::interval(HEARTBEAT);
    heartbeat.tick().await;

    loop {
        let deadline = current.as_ref().filter(|c| !c.expired).map(|c| c.deadline);
        tokio::select! {
            incoming = socket.recv() => {
                let raw = match incoming {
                    None | Some(Err(_)) | Some(Ok(Message::Close(_))) => break,
                    Some(Ok(Message::Pong(_))) => {
                        alive = true;
                        continue;
                    }
                    Some(Ok(Message::Ping(_))) => continue,
                    Some(Ok(Message::Text(t))) => t.as_bytes().to_vec(),
                    Some(Ok(Message::Binary(b))) => b.to_vec(),
                };
                let Ok(Value::Object(parsed)) = serde_json::from_slice::<Value>(&raw) else {
                    continue;
                };
                if handle_message(&mut socket, &state, &headers, &events, &mut current, parsed).await.is_err() {
                    break;
                }
            }
            Some(event) = events_rx.recv() => {
                if handle_event(&mut socket, &state, &events, &mut current, event).await.is_err() {
                    break;
                }
            }
            _ = heartbeat.tick() => {
                if !alive {
                    break;
                }
                alive = false;
                if socket.send(Message::Ping(Vec::new().into())).await.is_err() {
                    break;
                }
            }
            () = async { tokio::time::sleep_until(deadline.unwrap_or_else(Instant::now)).await }, if deadline.is_some() => {
                if let Some(c) = current.as_mut() {
                    c.expired = true;
                    tokio::spawn(c.stopper().stop());
                }
            }
        }
    }
    // `handleClose()`.
    if let Some(c) = current.take() {
        tokio::spawn(c.stopper().stop());
    }
}

/// `handleMessage()`: the keys in the order they came; `Err` ends the connection.
async fn handle_message(
    socket: &mut WebSocket,
    state: &AppState,
    headers: &HeaderMap,
    events: &mpsc::Sender<Event>,
    current: &mut Option<Running>,
    parsed: serde_json::Map<String, Value>,
) -> Result<(), ()> {
    for (key, value) in parsed {
        match key.as_str() {
            "ping" => socket.send(text("pong")).await.map_err(|_| ())?,
            "checkActive" => match current.as_ref() {
                Some(c) if value == "force" => {
                    tokio::spawn(c.stopper().stop());
                }
                active => socket
                    .send(text(if active.is_some() { "true" } else { "false" }))
                    .await
                    .map_err(|_| ())?,
            },
            "terminalToken" => {
                let Some(token) = token_shape_ok(&value) else {
                    const MESSAGE: &str = "Unauthorized: Invalid terminal token";
                    reject(socket, TOKEN_REJECTED, MESSAGE, Some(MESSAGE)).await;
                    return Err(());
                };
                let (state, headers, events, token) = (
                    state.clone(),
                    headers.clone(),
                    events.clone(),
                    token.to_owned(),
                );
                tokio::spawn(async move {
                    let session = redeem_for(&state, &headers, &token).await.map(Box::new);
                    let _ = events.send(Event::Redeemed(session)).await;
                });
            }
            "message" | "resize" | "pause" | "resume" => {
                let Some(c) = current.as_ref() else { continue };
                match key.as_str() {
                    "message" => {
                        if let Some(keys) = value.as_str() {
                            let _ = c.input.send(Input::Bytes(keys.as_bytes().to_vec()));
                        }
                    }
                    "resize" => {
                        let size = |name: &str, default: u16| {
                            value
                                .get(name)
                                .and_then(Value::as_f64)
                                .filter(|v| *v > 0.0)
                                .map_or(default, |v| v.min(f64::from(u16::MAX)) as u16)
                        };
                        let _ = c.input.send(Input::Resize(size("cols", 80), size("rows", 30)));
                    }
                    "pause" => {
                        let _ = c.paused.send(true);
                    }
                    _ => {
                        let _ = c.paused.send(false);
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

async fn handle_event(
    socket: &mut WebSocket,
    state: &AppState,
    events: &mpsc::Sender<Event>,
    current: &mut Option<Running>,
    event: Event,
) -> Result<(), ()> {
    const TOKEN_REFUSED: &str = "Unauthorized: Terminal token was rejected";
    match event {
        Event::Output(id, data) => {
            // A previous PTY can still be draining while its replacement runs.
            if current.as_ref().is_some_and(|c| c.id == id) {
                socket.send(text(data)).await.map_err(|_| ())?;
            }
        }
        Event::Exited(id) => {
            if current.as_ref().is_some_and(|c| c.id == id) {
                *current = None;
                socket.send(text("pty-exited")).await.map_err(|_| ())?;
            }
        }
        Event::Redeemed(Err(())) => {
            reject(socket, TOKEN_REJECTED, TOKEN_REFUSED, Some(TOKEN_REFUSED)).await;
            return Err(());
        }
        Event::Redeemed(Ok(session)) => return open(socket, state, events, current, session).await,
        Event::Replaced(false, _) => {
            socket.send(text("unprocessable")).await.map_err(|_| ())?;
        }
        Event::Replaced(true, session) => return open(socket, state, events, current, session).await,
    }
    Ok(())
}

/// `handleCommand()`: stops a running PTY first, then starts the one of this session.
async fn open(
    socket: &mut WebSocket,
    state: &AppState,
    events: &mpsc::Sender<Event>,
    current: &mut Option<Running>,
    session: Box<Session>,
) -> Result<(), ()> {
    if let Some(c) = current.as_ref() {
        let stop = c.stopper().stop();
        let events = events.clone();
        tokio::spawn(async move {
            let stopped = stop.await;
            let _ = events.send(Event::Replaced(stopped, session)).await;
        });
        return Ok(());
    }
    let Some(jobs) = state.jobs.as_ref() else {
        return Err(());
    };
    let Ok(invocation) = jobs
        .remote()
        .tty_invocation(&session.target, &session.command)
    else {
        return socket
            .send(text("Invalid SSH command: Unsupported SSH arguments"))
            .await
            .map_err(|_| ());
    };
    match spawn(invocation, session.attach, events.clone()) {
        Ok(running) => {
            *current = Some(running);
            socket.send(text("pty-ready")).await.map_err(|_| ())
        }
        Err(e) => {
            tracing::warn!(error = %e, "terminal: could not start ssh in a PTY");
            const REFUSED: &str = "Unauthorized: Terminal token was rejected";
            reject(socket, TOKEN_REJECTED, REFUSED, Some(REFUSED)).await;
            Err(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn output_keeps_a_split_character() {
        let mut pending = Vec::new();
        let bytes = "é€".as_bytes();
        assert_eq!(decode(&mut pending, &bytes[..1]), "");
        assert_eq!(decode(&mut pending, &bytes[1..3]), "é");
        assert_eq!(decode(&mut pending, &bytes[3..]), "€");
        assert!(pending.is_empty());
        assert_eq!(decode(&mut pending, b"a\xffb"), "a\u{fffd}b");
    }

    #[test]
    fn token_shape() {
        assert!(token_shape_ok(&Value::String("a".repeat(64))).is_some());
        assert!(token_shape_ok(&Value::String("a".repeat(63))).is_none());
        assert!(token_shape_ok(&Value::String(format!("{}-", "a".repeat(63)))).is_none());
        assert!(token_shape_ok(&serde_json::json!(5)).is_none());
    }

    #[test]
    fn origin_must_be_this_server() {
        let mut h = HeaderMap::new();
        h.insert(header::HOST, HeaderValue::from_static("coolify.test:8000"));
        assert!(same_origin(&h));
        h.insert(header::ORIGIN, HeaderValue::from_static("https://evil.test"));
        assert!(!same_origin(&h));
        h.insert(
            header::ORIGIN,
            HeaderValue::from_static("http://coolify.test:8000"),
        );
        assert!(same_origin(&h));
    }
}
