//! Error location tracking (copied from rusty-kaspa/core/src/error_location.rs).

use std::fmt::{self, Display, Formatter};

#[derive(Debug, Clone)]
pub struct ErrorLocation {
    file: String,
    line: String,
}

impl ErrorLocation {
    #[track_caller]
    pub fn capture() -> Self {
        let l = std::panic::Location::caller();
        Self {
            file: l.file().to_string(),
            line: l.line().to_string(),
        }
    }
}

impl Display for ErrorLocation {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.file, self.line)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_records_current_call_site() {
        let loc = ErrorLocation::capture();
        let s = loc.to_string();
        assert!(s.contains("common/src/error_location.rs"), "got: {s}");
    }

    #[test]
    fn is_clone_and_debug() {
        let loc = ErrorLocation::capture();
        let _clone = loc.clone();
        let _dbg = format!("{loc:?}");
    }
}
