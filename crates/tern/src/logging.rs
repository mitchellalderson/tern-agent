use tern_core::TernHome;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::EnvFilter;

pub const LOG_ENV: &str = "TERN_LOG";

/// Log to a daily-rotated file under `logs/`. Returns false (no logging) if
/// the logs directory doesn't exist yet.
///
/// Writes are synchronous, not buffered on a background thread: tern logs
/// little, and a buffered writer loses the last lines before a crash —
/// exactly the ones that explain it.
pub fn init(home: &TernHome) -> bool {
    let dir = home.logs_dir();
    if !dir.is_dir() {
        return false;
    }
    let Ok(appender) = RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix("tern")
        .filename_suffix("log")
        .max_log_files(14)
        .build(&dir)
    else {
        return false;
    };
    let filter = EnvFilter::try_from_env(LOG_ENV).unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(appender)
        .with_ansi(false)
        .try_init()
        .is_ok()
}
