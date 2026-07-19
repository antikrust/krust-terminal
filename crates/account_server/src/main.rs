//! account-server: lean account + settings-sync service for the fork.
//!
//! Configuration is environment-first so the systemd unit stays the whole
//! deployment story:
//!   ACCOUNT_SERVER_BIND    address to bind (default 0.0.0.0)
//!   ACCOUNT_SERVER_PORT    port (default 10101 — non-web on purpose)
//!   ACCOUNT_SERVER_DB      sqlite path (default ./account-server.sqlite3)
//!   ACCOUNT_SERVER_TLS_DIR where the self-signed certificate lives
//!                          (default: alongside the database)
//!   ACCOUNT_SERVER_TLS     set to 0/false to serve plain HTTP instead —
//!                          local testing only; see Transport::Plaintext

use std::net::SocketAddr;
use std::path::PathBuf;

fn main() -> anyhow::Result<()> {
    env_logger_init();

    let bind = std::env::var("ACCOUNT_SERVER_BIND").unwrap_or_else(|_| "0.0.0.0".to_string());
    let port: u16 = std::env::var("ACCOUNT_SERVER_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(10101);
    let db: PathBuf = std::env::var("ACCOUNT_SERVER_DB")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("account-server.sqlite3"));

    // TLS is the default; opting out is an explicit, deliberate act because
    // bearer tokens ride every request.
    let tls_enabled = !matches!(
        std::env::var("ACCOUNT_SERVER_TLS")
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "0" | "false" | "no"
    );
    let tls_dir: PathBuf = std::env::var("ACCOUNT_SERVER_TLS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            db.parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or_else(|| std::path::Path::new("."))
                .to_path_buf()
        });

    let addr: SocketAddr = format!("{bind}:{port}").parse()?;
    let state = account_server::open_state(&db)?;
    let transport = if tls_enabled {
        account_server::Transport::Tls { dir: tls_dir }
    } else {
        account_server::Transport::Plaintext
    };

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(account_server::serve(state, addr, transport))
}

/// Minimal stderr logging without pulling a logger crate into the workspace:
/// systemd captures stderr into the journal, which is all this needs.
fn env_logger_init() {
    struct StderrLog;
    impl log::Log for StderrLog {
        fn enabled(&self, metadata: &log::Metadata) -> bool {
            metadata.level() <= log::Level::Info
        }
        fn log(&self, record: &log::Record) {
            if self.enabled(record.metadata()) {
                eprintln!(
                    "{} [{}] {}",
                    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ"),
                    record.level(),
                    record.args()
                );
            }
        }
        fn flush(&self) {}
    }
    let _ = log::set_logger(&StderrLog);
    log::set_max_level(log::LevelFilter::Info);
}
