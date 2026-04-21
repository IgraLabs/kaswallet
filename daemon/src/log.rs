use crate::args::LogsLevel;
use common::error_location::ErrorLocation;
use common::errors::ConfigError;
use std::path::Path;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer, fmt};

pub struct LogGuards {
    pub _general: WorkerGuard,
    pub _err: WorkerGuard,
}

pub fn init_log(logs_path: &str, logs_level: &LogsLevel) -> Result<LogGuards, ConfigError> {
    let dir = Path::new(logs_path);
    if !dir.exists() {
        std::fs::create_dir_all(dir).map_err(|e| ConfigError::InvalidPath {
            path: logs_path.to_string(),
            reason: e.to_string(),
            location: ErrorLocation::capture(),
        })?;
    }

    let general_appender = rolling::daily(dir, "kaswallet.log");
    let err_appender = rolling::daily(dir, "kaswallet.err.log");
    let (general_writer, general_guard) = tracing_appender::non_blocking(general_appender);
    let (err_writer, err_guard) = tracing_appender::non_blocking(err_appender);

    let level: LevelFilter = logs_level.into();

    let stdout_layer = fmt::layer().with_writer(std::io::stdout).with_filter(level);
    let file_layer = fmt::layer()
        .with_writer(general_writer)
        .with_ansi(false)
        .with_filter(level);
    let err_layer = fmt::layer()
        .json()
        .with_writer(err_writer)
        .with_ansi(false)
        .with_filter(LevelFilter::WARN);

    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with(stdout_layer)
        .with(file_layer)
        .with(err_layer)
        .try_init()
        .map_err(|e| ConfigError::InvalidLogLevel {
            value: e.to_string(),
            location: ErrorLocation::capture(),
        })?;

    Ok(LogGuards {
        _general: general_guard,
        _err: err_guard,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::args::LogsLevel;

    #[test]
    fn init_log_on_valid_path_returns_guard() {
        let tmp = tempfile::tempdir().unwrap();
        let guard = init_log(tmp.path().to_str().unwrap(), &LogsLevel::Info);
        assert!(guard.is_ok());
    }
}
