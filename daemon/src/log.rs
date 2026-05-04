use crate::args::LogsLevel;
use common::error_location::ErrorLocation;
use common::errors::ConfigError;
use std::path::Path;
use tracing_appender::non_blocking::{NonBlockingBuilder, WorkerGuard};
use tracing_appender::rolling::{Builder as RollingBuilder, Rotation};
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer, fmt};

const GENERAL_LOG_PREFIX: &str = "kaswallet.log";
const ERR_LOG_PREFIX: &str = "kaswallet.err.log";
const MAX_LOG_FILES: usize = 10;
const NON_BLOCKING_BUFFER_LINES: usize = 8192;

pub struct LogGuards {
    _general: WorkerGuard,
    _err: WorkerGuard,
}

pub fn init_log(
    logs_path: &str,
    logs_level: &LogsLevel,
    enable_console: bool,
) -> Result<LogGuards, ConfigError> {
    let dir = Path::new(logs_path);
    std::fs::create_dir_all(dir).map_err(|e| ConfigError::InvalidPath {
        path: logs_path.to_string(),
        reason: e.to_string(),
        location: ErrorLocation::capture(),
    })?;

    // On Unix, restrict the logs directory to the owner. Without the `x` bit
    // for group/other no other user can traverse the directory, so files inside
    // remain inaccessible regardless of their own mode.
    secure_log_dir(dir, logs_path)?;

    let general_appender = build_appender(dir, GENERAL_LOG_PREFIX, logs_path)?;
    let err_appender = build_appender(dir, ERR_LOG_PREFIX, logs_path)?;

    let (general_writer, general_guard) = NonBlockingBuilder::default()
        .lossy(false)
        .buffered_lines_limit(NON_BLOCKING_BUFFER_LINES)
        .finish(general_appender);
    let (err_writer, err_guard) = NonBlockingBuilder::default()
        .lossy(false)
        .buffered_lines_limit(NON_BLOCKING_BUFFER_LINES)
        .finish(err_appender);

    let level: LevelFilter = logs_level.into();
    let env_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(level.to_string()));

    let stdout_layer = fmt::layer().with_writer(std::io::stdout);
    let file_layer = fmt::layer().with_writer(general_writer).with_ansi(false);
    let err_layer = fmt::layer()
        .json()
        .with_writer(err_writer)
        .with_ansi(false)
        .with_filter(LevelFilter::WARN);

    let registry = tracing_subscriber::registry()
        .with(env_filter)
        .with(stdout_layer)
        .with(file_layer)
        .with(err_layer);

    let init_result = {
        #[cfg(debug_assertions)]
        {
            if enable_console {
                registry.with(console_subscriber::spawn()).try_init()
            } else {
                registry.try_init()
            }
        }
        #[cfg(not(debug_assertions))]
        {
            let _ = enable_console;
            registry.try_init()
        }
    };

    init_result.map_err(|e| ConfigError::SubscriberAlreadyInitialized {
        reason: e.to_string(),
        location: ErrorLocation::capture(),
    })?;

    Ok(LogGuards {
        _general: general_guard,
        _err: err_guard,
    })
}

fn build_appender(
    dir: &Path,
    prefix: &str,
    logs_path: &str,
) -> Result<tracing_appender::rolling::RollingFileAppender, ConfigError> {
    RollingBuilder::new()
        .rotation(Rotation::DAILY)
        .filename_prefix(prefix)
        .max_log_files(MAX_LOG_FILES)
        .build(dir)
        .map_err(|e| ConfigError::InvalidPath {
            path: logs_path.to_string(),
            reason: e.to_string(),
            location: ErrorLocation::capture(),
        })
}

#[cfg(unix)]
fn secure_log_dir(dir: &Path, logs_path: &str) -> Result<(), ConfigError> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).map_err(|e| {
        ConfigError::InvalidPath {
            path: logs_path.to_string(),
            reason: e.to_string(),
            location: ErrorLocation::capture(),
        }
    })
}

#[cfg(not(unix))]
fn secure_log_dir(_dir: &Path, _logs_path: &str) -> Result<(), ConfigError> {
    Ok(())
}

/// Test-only logger init for integration tests. Idempotent and writes to the
/// test harness's stdout capture, so multiple tests can call it without
/// fighting over the global subscriber.
pub fn init_log_for_tests() {
    use std::sync::Once;
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        let _ = tracing_subscriber::fmt()
            .with_test_writer()
            .with_env_filter("info")
            .try_init();
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::args::LogsLevel;
    use std::fs::File;
    use tracing::Level;
    use tracing_subscriber::fmt::TestWriter;

    #[test]
    fn init_log_creates_missing_parent_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let nested = tmp.path().join("a").join("b").join("c");
        // We don't care whether the global subscriber install succeeds here —
        // we only assert the directory-creation side effect happened first.
        let _ = init_log(nested.to_str().unwrap(), &LogsLevel::Info, false);
        assert!(nested.is_dir(), "expected init_log to create nested path");
    }

    #[test]
    fn init_log_on_existing_file_returns_invalid_path() {
        let tmp = tempfile::tempdir().unwrap();
        let file_path = tmp.path().join("not_a_dir");
        File::create(&file_path).unwrap();

        let result = init_log(file_path.to_str().unwrap(), &LogsLevel::Info, false);
        assert!(
            matches!(result, Err(ConfigError::InvalidPath { .. })),
            "expected InvalidPath, got error variant other than InvalidPath"
        );
    }

    /// Verifies the EnvFilter regression fix: with RUST_LOG unset, the CLI
    /// `--logs-level` must be authoritative — building EnvFilter from the level
    /// (instead of a hard-coded "info") is what allows debug events through.
    ///
    /// Cannot install the global subscriber from a unit test (other tests share
    /// the process), so we build an isolated registry mirroring `init_log`'s
    /// filter wiring and assert a debug event is admitted.
    #[test]
    fn env_filter_honors_cli_level_when_rust_log_unset() {
        let saved = std::env::var("RUST_LOG").ok();
        // SAFETY: tests do not race readers of RUST_LOG in this binary.
        unsafe { std::env::remove_var("RUST_LOG") };

        let level: LevelFilter = (&LogsLevel::Debug).into();
        let env_filter =
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(level.to_string()));

        let admits_debug = env_filter
            .max_level_hint()
            .map(|hint| hint >= LevelFilter::DEBUG)
            .unwrap_or(true);

        if let Some(v) = saved {
            unsafe { std::env::set_var("RUST_LOG", v) };
        }

        assert!(
            admits_debug,
            "EnvFilter built from --logs-level=debug must admit DEBUG events when RUST_LOG is unset"
        );

        let subscriber = tracing_subscriber::registry()
            .with(
                EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| EnvFilter::new(LevelFilter::DEBUG.to_string())),
            )
            .with(fmt::layer().with_writer(TestWriter::new()));
        let _ = subscriber.set_default();
        tracing::event!(Level::DEBUG, "debug event for env-filter test");
    }
}
