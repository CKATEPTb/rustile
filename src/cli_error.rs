//! Formats OS failures for the English CLI while retaining the Win32 error code.

use std::io::Error;

/// Windows formats OS errors in the user's display language. Use Rust's
/// English error category and keep the Win32 code for diagnosis.
pub(crate) fn io(error: Error) -> String {
    match error.raw_os_error() {
        Some(code) => format!("{} (Windows error {code})", error.kind()),
        None => error.to_string(),
    }
}
