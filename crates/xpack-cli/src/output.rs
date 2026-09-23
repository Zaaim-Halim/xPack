//! Printing results.
//!
//! Results go to standard output so they can be piped; everything else goes to
//! standard error. `--json` exists because parsing human-readable output is
//! how scripts break on the next release.

use serde::Serialize;
use xpack_core::{Error, Result};

/// Prints a value as pretty JSON.
pub(crate) fn json<T: Serialize>(value: &T) -> Result<()> {
    let text = serde_json::to_string_pretty(value).map_err(|e| Error::json("command output", e))?;
    xpack_core::outln!("{text}");
    Ok(())
}

/// Prints a labelled field, aligned for reading in a terminal.
pub(crate) fn field(label: &str, value: impl std::fmt::Display) {
    xpack_core::outln!("{label:<14} {value}");
}
