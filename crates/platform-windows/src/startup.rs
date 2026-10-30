// SPDX-License-Identifier: Apache-2.0
//! Per-user HKCU Run registration; never a Session-0 service. The registration
//! does not launch a process immediately and needs no administrator rights.
#[cfg(windows)]
pub use crate::win::startup::{Status, install, status, uninstall};

/// Construct the exact Run value; no shell, environment expansion or arguments
/// from callers. Embedded quotes and controls cannot be executable filenames.
pub fn command_line(executable: &str) -> std::io::Result<String> {
    let bytes = executable.as_bytes();
    if bytes.len() < 4
        || !bytes[0].is_ascii_alphabetic()
        || &bytes[1..3] != b":\\"
        || executable.chars().any(|c| c.is_control() || c == '"')
        || executable.ends_with('\\')
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "startup requires an absolute local executable path",
        ));
    }
    let result = format!("\"{executable}\" run");
    if result.encode_utf16().count() > 260 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "startup command exceeds the Windows Run limit",
        ));
    }
    Ok(result)
}
#[cfg(test)]
mod tests {
    #[test]
    fn quotes_spaces_and_preserves_literal_percent() {
        assert_eq!(
            super::command_line(r"C:\Program Files\50%\ophio.exe").unwrap(),
            "\"C:\\Program Files\\50%\\ophio.exe\" run"
        );
        for path in [
            "relative.exe",
            "C:\\bad\".exe",
            "C:\\bad\n.exe",
            "\\\\server\\share\\app.exe",
        ] {
            assert!(super::command_line(path).is_err());
        }
    }
}
