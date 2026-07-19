//! account-server: lean account + settings-sync service for the fork.
//!
//! Configuration is environment-first so the systemd unit stays the whole
//! deployment story:
//!   ACCOUNT_SERVER_BIND  address to bind (default 0.0.0.0)
//!   ACCOUNT_SERVER_PORT  port (default 10101 — non-web on purpose)
//!   ACCOUNT_SERVER_DB    sqlite path (default ./account-server.sqlite3)

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

    let addr: SocketAddr = format!("{bind}:{port}").parse()?;
    let state = account_server::open_state(&db)?;

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(account_server::serve(state, addr))
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
