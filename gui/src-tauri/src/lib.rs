//! TNSM Relay GUI — Tauri backend.
//!
//! Runs the relay IN-PROCESS so one app gives you config + start/stop + a
//! live connection list. Uses the same `tnsm-relay-core` as the headless
//! binary, so behavior is identical. Best run on a machine with a desktop
//! (your PC for testing, or a desktop-VPS); for a headless VPS use the
//! headless binary as the always-on service and use this GUI locally.

use std::sync::Arc;
use tnsm_relay_core::{run, RelayConfig, RelayStats, StatsSnapshot};
use tokio::sync::{oneshot, Mutex};

/// Shared app state: the live stats and the current shutdown sender (Some
/// while running).
struct AppState {
    stats: RelayStats,
    shutdown: Mutex<Option<oneshot::Sender<()>>>,
    config_path: String,
}

fn config_path() -> String {
    // Store next to the executable for portability.
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("relay.toml")))
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| "relay.toml".to_string())
}

#[tauri::command]
async fn get_config(state: tauri::State<'_, Arc<AppState>>) -> Result<RelayConfig, String> {
    Ok(RelayConfig::load(&state.config_path))
}

#[tauri::command]
async fn save_config(
    state: tauri::State<'_, Arc<AppState>>,
    config: RelayConfig,
) -> Result<(), String> {
    config.save(&state.config_path).map_err(|e| e.to_string())
}

#[tauri::command]
async fn start_relay(
    state: tauri::State<'_, Arc<AppState>>,
    config: RelayConfig,
) -> Result<(), String> {
    // Persist whatever the UI passed, then start.
    let _ = config.save(&state.config_path);

    let mut guard = state.shutdown.lock().await;
    if guard.is_some() {
        return Err("relay already running".into());
    }
    let (tx, rx) = oneshot::channel::<()>();
    let stats = state.stats.clone();
    let cfg = config.clone();

    // Bind synchronously enough to report failure: spawn, but first try a
    // quick bind check by letting run() report via a result channel.
    let (ready_tx, ready_rx) = oneshot::channel::<Result<(), String>>();
    tokio::spawn(async move {
        // run() binds immediately; wrap so we can report the bind result.
        // We can't easily intercept the internal bind result without
        // changing core, so we do a pre-bind probe here.
        match tokio::net::TcpListener::bind(&cfg.bind).await {
            Ok(l) => {
                drop(l); // release; core will rebind. Tiny TOCTOU window is
                         // acceptable for a single-admin control panel.
                let _ = ready_tx.send(Ok(()));
                let _ = run(cfg, stats, rx).await;
            }
            Err(e) => {
                let _ = ready_tx.send(Err(format!("bind {}: {e}", cfg.bind)));
            }
        }
    });

    match ready_rx.await {
        Ok(Ok(())) => {
            *guard = Some(tx);
            Ok(())
        }
        Ok(Err(e)) => Err(e),
        Err(_) => Err("relay task failed to start".into()),
    }
}

#[tauri::command]
async fn stop_relay(state: tauri::State<'_, Arc<AppState>>) -> Result<(), String> {
    let mut guard = state.shutdown.lock().await;
    if let Some(tx) = guard.take() {
        let _ = tx.send(());
        Ok(())
    } else {
        Err("relay not running".into())
    }
}

#[tauri::command]
async fn get_stats(state: tauri::State<'_, Arc<AppState>>) -> Result<StatsSnapshot, String> {
    Ok(state.stats.snapshot().await)
}

// ── Update check (notice only — no auto-install) ────────────────────────────

/// Public version manifest the app checks. Replace with YOUR public URL —
/// e.g. a GitHub Pages file, a public Gist raw URL, or a release asset in a
/// public "releases-only" repo. Your source repo can stay private; only this
/// tiny JSON needs to be publicly readable. Expected shape:
///   { "version": "0.2.0", "url": "https://.../download", "notes": "..." }
const UPDATE_MANIFEST_URL: &str = "https://example.com/tnsm-relay/latest.json";

/// This build's version (kept in sync with Cargo/tauri.conf version).
const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(serde::Serialize, serde::Deserialize)]
struct Manifest {
    version: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    notes: String,
}

#[derive(serde::Serialize)]
struct UpdateInfo {
    current: String,
    latest: String,
    update_available: bool,
    url: String,
    notes: String,
}

/// Parse "a.b.c" into a comparable tuple. Non-numeric / missing parts are 0.
fn parse_semver(v: &str) -> (u64, u64, u64) {
    let v = v.trim().trim_start_matches('v');
    let mut it = v.split('.').map(|p| {
        p.chars().take_while(|c| c.is_ascii_digit()).collect::<String>().parse::<u64>().unwrap_or(0)
    });
    (it.next().unwrap_or(0), it.next().unwrap_or(0), it.next().unwrap_or(0))
}

#[tauri::command]
async fn check_update() -> Result<UpdateInfo, String> {
    // Fetch the public manifest. Uses the http plugin's reqwest client so we
    // don't fight webview CORS/CSP.
    let resp = tauri_plugin_http::reqwest::Client::new()
        .get(UPDATE_MANIFEST_URL)
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|e| format!("fetch manifest: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("manifest HTTP {}", resp.status()));
    }
    // The http plugin's reqwest doesn't enable the `json` feature, so read
    // the body as text and parse with serde_json (already a dependency).
    let body = resp.text().await.map_err(|e| format!("read manifest body: {e}"))?;
    let manifest: Manifest =
        serde_json::from_str(&body).map_err(|e| format!("parse manifest: {e}"))?;

    let cur = parse_semver(APP_VERSION);
    let lat = parse_semver(&manifest.version);
    Ok(UpdateInfo {
        current: APP_VERSION.to_string(),
        latest: manifest.version,
        update_available: lat > cur,
        url: manifest.url,
        notes: manifest.notes,
    })
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run_app() {
    let state = Arc::new(AppState {
        stats: RelayStats::new(),
        shutdown: Mutex::new(None),
        config_path: config_path(),
    });

    tauri::Builder::default()
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_http::init())
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            get_config,
            save_config,
            start_relay,
            stop_relay,
            get_stats,
            check_update
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
