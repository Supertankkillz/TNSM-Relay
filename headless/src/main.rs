//! TNSM relay — headless service binary.
//!
//! Always-on rendezvous relay for production (run on your VPS as a systemd /
//! NSSM service). Thin wrapper over `tnsm-relay-core`; the GUI uses the same
//! core so behavior never diverges.
//!
//! Usage:  tnsm-relay [config.toml]   (defaults to ./relay.toml, then builtins)

use tnsm_relay_core::{run, RelayConfig, RelayStats};

fn main() {
    // Subcommand dispatch (sync, before the async runtime).
    let args: Vec<String> = std::env::args().collect();
    if args.len() >= 2 && args[1] == "gen-hash" {
        gen_hash(args.get(2).cloned());
        return;
    }
    // Normal service mode.
    serve(args.into_iter().nth(1));
}

/// `tnsm-relay gen-hash [password]` — print an Argon2 PHC hash of the given
/// password (or one read from stdin if omitted) for pasting into the relay
/// config / a future accounts file. The plaintext is never stored; only the
/// printed hash is. Future per-user accounts will verify against hashes like
/// this. (With a single operator, the config `shared_secret` already gates
/// the relay; this command future-proofs the per-user path.)
fn gen_hash(arg: Option<String>) {
    use argon2::password_hash::{rand_core::OsRng, PasswordHasher, SaltString};
    use argon2::Argon2;

    let password = match arg {
        Some(p) => p,
        None => {
            eprint!("Password: ");
            use std::io::Write;
            let _ = std::io::stderr().flush();
            let mut line = String::new();
            if std::io::stdin().read_line(&mut line).is_err() {
                eprintln!("failed to read password");
                std::process::exit(1);
            }
            line.trim_end_matches(['\r', '\n']).to_string()
        }
    };
    if password.is_empty() {
        eprintln!("empty password; aborting");
        std::process::exit(1);
    }
    let salt = SaltString::generate(&mut OsRng);
    match Argon2::default().hash_password(password.as_bytes(), &salt) {
        Ok(h) => {
            // Print ONLY the hash on stdout so it can be piped/copied cleanly.
            println!("{}", h);
            eprintln!("\n^ paste this hash into your relay config (never the plaintext).");
        }
        Err(e) => {
            eprintln!("hash error: {e}");
            std::process::exit(1);
        }
    }
}

#[tokio::main]
async fn serve(config_arg: Option<String>) {
    let path = config_arg.unwrap_or_else(|| "relay.toml".to_string());
    let cfg = RelayConfig::load(&path);
    eprintln!("[relay] config: bind={} secret={}",
        cfg.bind, if cfg.shared_secret.is_empty() { "off" } else { "on" });

    let stats = RelayStats::new();
    let (_tx, rx) = tokio::sync::oneshot::channel::<()>();

    let stats_for_log = stats.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            let s = stats_for_log.snapshot().await;
            eprintln!("[relay] waiting_hosts={} active_sessions={} total_sessions={} rejections={}",
                s.waiting_hosts.len(), s.active_sessions.len(), s.total_sessions, s.total_rejections);
        }
    });

    eprintln!("[relay] starting on {}", cfg.bind);
    match run(cfg, stats, rx).await {
        Ok(()) => eprintln!("[relay] stopped"),
        Err(e) => { eprintln!("[relay] fatal: {e}"); std::process::exit(1); }
    }
}
