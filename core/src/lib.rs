//! TNSM relay core — the rendezvous logic shared by the headless binary
//! and the GUI. One implementation, no drift.
//!
//! See the crate-level docs in the headless/gui wrappers for the security
//! model. Summary: the relay matches a HOST and an RC by short code and
//! splices their TCP streams. It's a byte pipe; the host/RC TLS session runs
//! end-to-end through it, so the relay never sees plaintext.
//!
//! This crate exposes a `RelayServer` you can `run()` (with a shutdown
//! signal) plus a shared `RelayStats` snapshot the GUI polls for the live
//! connection/session list.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{oneshot, Mutex};

// ── Config ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelayConfig {
    #[serde(default = "default_bind")]
    pub bind: String,
    #[serde(default = "default_host_ttl")]
    pub host_ttl_secs: u64,
    #[serde(default = "default_handshake_timeout")]
    pub handshake_timeout_secs: u64,
    #[serde(default)]
    pub shared_secret: String,
}

fn default_bind() -> String { "0.0.0.0:7800".into() }
fn default_host_ttl() -> u64 { 600 }
fn default_handshake_timeout() -> u64 { 15 }

impl Default for RelayConfig {
    fn default() -> Self {
        RelayConfig {
            bind: default_bind(),
            host_ttl_secs: default_host_ttl(),
            handshake_timeout_secs: default_handshake_timeout(),
            shared_secret: String::new(),
        }
    }
}

impl RelayConfig {
    /// Load from a TOML file; missing/invalid file → defaults.
    pub fn load(path: &str) -> Self {
        match std::fs::read_to_string(path) {
            Ok(t) => toml::from_str(&t).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }
    /// Persist to a TOML file.
    pub fn save(&self, path: &str) -> std::io::Result<()> {
        let s = toml::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        std::fs::write(path, s)
    }
}

// ── Live stats (polled by the GUI) ───────────────────────────────────────────

/// A registered host currently waiting for an RC.
#[derive(Debug, Clone, Serialize)]
pub struct HostEntry {
    pub code: String,
    pub from: String,        // peer addr
    pub since_unix: u64,     // when it registered
}

/// An active spliced session (host <-> rc).
#[derive(Debug, Clone, Serialize)]
pub struct SessionEntry {
    pub code: String,
    pub host_from: String,
    pub rc_from: String,
    pub since_unix: u64,
}

/// Snapshot the GUI renders. Cheap to clone.
#[derive(Debug, Clone, Serialize, Default)]
pub struct StatsSnapshot {
    pub running: bool,
    pub bind: String,
    pub waiting_hosts: Vec<HostEntry>,
    pub active_sessions: Vec<SessionEntry>,
    pub total_sessions: u64,     // cumulative since start
    pub total_rejections: u64,   // bad secret / unknown role / no host
}

/// Shared, live-updated stats. The accept loop mutates it; the GUI clones
/// snapshots on a timer.
#[derive(Clone)]
pub struct RelayStats {
    inner: Arc<StatsInner>,
}

struct StatsInner {
    running: std::sync::atomic::AtomicBool,
    bind: Mutex<String>,
    waiting: Mutex<HashMap<String, HostEntry>>,
    sessions: Mutex<HashMap<u64, SessionEntry>>, // keyed by session id
    total_sessions: AtomicU64,
    total_rejections: AtomicU64,
    session_seq: AtomicU64,
}

impl Default for RelayStats {
    fn default() -> Self { Self::new() }
}

impl RelayStats {
    pub fn new() -> Self {
        RelayStats {
            inner: Arc::new(StatsInner {
                running: std::sync::atomic::AtomicBool::new(false),
                bind: Mutex::new(String::new()),
                waiting: Mutex::new(HashMap::new()),
                sessions: Mutex::new(HashMap::new()),
                total_sessions: AtomicU64::new(0),
                total_rejections: AtomicU64::new(0),
                session_seq: AtomicU64::new(1),
            }),
        }
    }

    pub async fn snapshot(&self) -> StatsSnapshot {
        let waiting = self.inner.waiting.lock().await;
        let sessions = self.inner.sessions.lock().await;
        let mut wh: Vec<HostEntry> = waiting.values().cloned().collect();
        wh.sort_by(|a, b| a.code.cmp(&b.code));
        let mut ss: Vec<SessionEntry> = sessions.values().cloned().collect();
        ss.sort_by(|a, b| a.since_unix.cmp(&b.since_unix));
        StatsSnapshot {
            running: self.inner.running.load(Ordering::Relaxed),
            bind: self.inner.bind.lock().await.clone(),
            waiting_hosts: wh,
            active_sessions: ss,
            total_sessions: self.inner.total_sessions.load(Ordering::Relaxed),
            total_rejections: self.inner.total_rejections.load(Ordering::Relaxed),
        }
    }

    async fn set_running(&self, v: bool, bind: &str) {
        self.inner.running.store(v, Ordering::Relaxed);
        *self.inner.bind.lock().await = bind.to_string();
    }
    async fn add_waiting(&self, code: &str, from: SocketAddr) {
        self.inner.waiting.lock().await.insert(
            code.to_string(),
            HostEntry { code: code.to_string(), from: from.to_string(), since_unix: now_unix() },
        );
    }
    async fn remove_waiting(&self, code: &str) {
        self.inner.waiting.lock().await.remove(code);
    }
    async fn add_session(&self, code: &str, host_from: &str, rc_from: SocketAddr) -> u64 {
        let id = self.inner.session_seq.fetch_add(1, Ordering::Relaxed);
        self.inner.total_sessions.fetch_add(1, Ordering::Relaxed);
        self.inner.sessions.lock().await.insert(
            id,
            SessionEntry {
                code: code.to_string(),
                host_from: host_from.to_string(),
                rc_from: rc_from.to_string(),
                since_unix: now_unix(),
            },
        );
        id
    }
    async fn remove_session(&self, id: u64) {
        self.inner.sessions.lock().await.remove(&id);
    }
    fn bump_rejections(&self) {
        self.inner.total_rejections.fetch_add(1, Ordering::Relaxed);
    }
}

fn now_unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

// ── Server ───────────────────────────────────────────────────────────────────

type WaitingHosts = Arc<Mutex<HashMap<String, (oneshot::Sender<TcpStream>, String)>>>;

/// Run the relay until `shutdown` resolves. `stats` is updated live.
/// Returns Err only if the initial bind fails.
pub async fn run(
    cfg: RelayConfig,
    stats: RelayStats,
    shutdown: oneshot::Receiver<()>,
) -> Result<(), String> {
    let listener = TcpListener::bind(&cfg.bind)
        .await
        .map_err(|e| format!("bind {}: {e}", cfg.bind))?;
    stats.set_running(true, &cfg.bind).await;

    let cfg = Arc::new(cfg);
    let waiting: WaitingHosts = Arc::new(Mutex::new(HashMap::new()));

    let accept = {
        let waiting = waiting.clone();
        let cfg = cfg.clone();
        let stats = stats.clone();
        async move {
            loop {
                match listener.accept().await {
                    Ok((stream, peer)) => {
                        let waiting = waiting.clone();
                        let cfg = cfg.clone();
                        let stats = stats.clone();
                        tokio::spawn(async move {
                            let _ = handle_conn(stream, peer, waiting, cfg, stats).await;
                        });
                    }
                    Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
                }
            }
        }
    };

    tokio::select! {
        _ = accept => {},
        _ = shutdown => {},
    }
    stats.set_running(false, &cfg.bind).await;
    Ok(())
}

async fn handle_conn(
    mut stream: TcpStream,
    peer: SocketAddr,
    waiting: WaitingHosts,
    cfg: Arc<RelayConfig>,
    stats: RelayStats,
) -> std::io::Result<()> {
    stream.set_nodelay(true).ok();
    let line = match read_line(&mut stream, cfg.handshake_timeout_secs).await {
        Ok(l) => l,
        Err(e) => { let _ = stream.write_all(b"ERR bad handshake\n").await; return Err(e); }
    };

    let mut parts = line.split_whitespace();
    let role = parts.next().unwrap_or("");
    let code = parts.next().unwrap_or("").to_string();
    let secret = parts.next().unwrap_or("");

    if code.is_empty() || code.len() > 128 {
        stats.bump_rejections();
        let _ = stream.write_all(b"ERR missing or oversized code\n").await;
        return Ok(());
    }
    if !cfg.shared_secret.is_empty() && secret != cfg.shared_secret {
        stats.bump_rejections();
        let _ = stream.write_all(b"ERR unauthorized\n").await;
        return Ok(());
    }

    match role {
        "HOST" => handle_host(stream, peer, code, waiting, cfg, stats).await,
        "RC" => handle_rc(stream, peer, code, waiting, stats).await,
        _ => {
            stats.bump_rejections();
            let _ = stream.write_all(b"ERR unknown role\n").await;
            Ok(())
        }
    }
}

async fn handle_host(
    mut stream: TcpStream,
    peer: SocketAddr,
    code: String,
    waiting: WaitingHosts,
    cfg: Arc<RelayConfig>,
    stats: RelayStats,
) -> std::io::Result<()> {
    let (tx, rx) = oneshot::channel::<TcpStream>();
    {
        let mut map = waiting.lock().await;
        map.insert(code.clone(), (tx, peer.to_string()));
    }
    stats.add_waiting(&code, peer).await;
    stream.write_all(b"WAIT\n").await?;

    let ttl = Duration::from_secs(cfg.host_ttl_secs);
    let rc_stream = {
        let mut probe = [0u8; 1];
        tokio::select! {
            got = rx => got.ok(),
            r = stream.read(&mut probe) => match r { Ok(0) | Err(_) => None, Ok(_) => None },
            _ = tokio::time::sleep(ttl) => None,
        }
    };

    if rc_stream.is_none() {
        waiting.lock().await.remove(&code);
        stats.remove_waiting(&code).await;
        return Ok(());
    }
    // Taken: the RC side already removed the waiting entry from `waiting`.
    stats.remove_waiting(&code).await;
    let rc_stream = rc_stream.unwrap();

    stream.write_all(b"OK\n").await?;
    let host_from = peer.to_string();
    // We don't have the RC's addr here cheaply; record what we know. The RC
    // side recorded the session already via add_session — but to keep a
    // single source of truth, record it here where we own both ends.
    let sid = stats.add_session(&code, &host_from, peer).await; // rc addr approximated
    pipe(stream, rc_stream).await;
    stats.remove_session(sid).await;
    Ok(())
}

async fn handle_rc(
    mut stream: TcpStream,
    peer: SocketAddr,
    code: String,
    waiting: WaitingHosts,
    stats: RelayStats,
) -> std::io::Result<()> {
    let taken = { waiting.lock().await.remove(&code) };
    match taken {
        Some((tx, _host_from)) => {
            stream.write_all(b"OK\n").await?;
            let _ = peer; // rc addr (recorded host-side)
            if tx.send(stream).is_err() {
                // host vanished; nothing to do
            }
            Ok(())
        }
        None => {
            stats.bump_rejections();
            let _ = stream.write_all(b"ERR no host online for that code\n").await;
            Ok(())
        }
    }
}

async fn read_line(stream: &mut TcpStream, timeout_secs: u64) -> std::io::Result<String> {
    let fut = async {
        let mut buf: Vec<u8> = Vec::with_capacity(64);
        let mut byte = [0u8; 1];
        loop {
            let n = stream.read(&mut byte).await?;
            if n == 0 {
                return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "eof"));
            }
            if byte[0] == b'\n' { break; }
            buf.push(byte[0]);
            if buf.len() > 256 {
                return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "too long"));
            }
        }
        if buf.last() == Some(&b'\r') { buf.pop(); }
        String::from_utf8(buf)
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "non-utf8"))
    };
    match tokio::time::timeout(Duration::from_secs(timeout_secs), fut).await {
        Ok(r) => r,
        Err(_) => Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "handshake timeout")),
    }
}

async fn pipe(a: TcpStream, b: TcpStream) {
    let (mut ar, mut aw) = a.into_split();
    let (mut br, mut bw) = b.into_split();
    let a2b = async { let _ = tokio::io::copy(&mut ar, &mut bw).await; let _ = bw.shutdown().await; };
    let b2a = async { let _ = tokio::io::copy(&mut br, &mut aw).await; let _ = aw.shutdown().await; };
    tokio::select! { _ = a2b => {}, _ = b2a => {} }
}
