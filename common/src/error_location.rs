//! Source-location capture for typed errors.
//!
//! Stores `&'static str` and `u32` directly from `std::panic::Location::caller()`
//! — no allocations, `Copy`, can live inside error variants without forcing
//! `Clone` requirements on the enum tree.

use std::fmt::{self, Display, Formatter};

#[derive(Debug, Clone, Copy)]
pub struct ErrorLocation {
    file: &'static str,
    line: u32,
}

impl ErrorLocation {
    #[track_caller]
    pub fn capture() -> Self {
        let l = std::panic::Location::caller();
        Self {
            file: l.file(),
            line: l.line(),
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
    fn is_copy_and_debug() {
        let loc = ErrorLocation::capture();
        let _copy = loc;
        let _again = loc;
        let _dbg = format!("{loc:?}");
    }
}
