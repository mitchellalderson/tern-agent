use tern_core::TernHome;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::EnvFilter;

pub const LOG_ENV: &str = "TERN_LOG";

/// Log to a daily-rotated file under `logs/`. Returns `None` (no logging) if
/// the logs directory doesn't exist yet. Keep the guard alive until exit.
pub fn init(home: &TernHome) -> Option<WorkerGuard> {
    let dir = home.logs_dir();
    if !dir.is_dir() {
        return None;
    }
    let appender = RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix("tern")
        .filename_suffix("log")
        .max_log_files(14)
        .build(&dir)
        .ok()?;
    let (writer, guard) = tracing_appender::non_blocking(appender);
    let filter = EnvFilter::try_from_env(LOG_ENV).unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(writer)
        .with_ansi(false)
        .try_init()
        .ok()?;
    Some(guard)
}
