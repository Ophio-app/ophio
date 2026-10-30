// SPDX-License-Identifier: Apache-2.0
//! Owner-only storage on local Windows volumes. Reparse points at every path
//! component, non-files and hard links are rejected. Reads have a hard bound.
#[cfg(windows)]
pub use crate::win::fs::{atomic_write, private_dir, private_open, protected_read};

/// Whether Win32 opens a path component under its own name. It would read a
/// colon as an alternate data stream, drop a trailing dot or space, and open
/// a device for a reserved name such as `NUL` or `com1.txt`.
#[cfg(any(windows, test))]
pub(crate) fn plain_component(name: &str) -> bool {
    !(name.contains(':') || name.ends_with(['.', ' ']) || ports::library::reserved_on_windows(name))
}

#[cfg(test)]
mod tests {
    use super::plain_component;

    #[test]
    fn components_win32_would_rename_or_open_as_a_device_are_refused() {
        for name in [
            "CON",
            "nul",
            "Aux.json",
            "PRN.tmp",
            "COM1",
            "com9.log",
            "LPT1",
            "lpt9.dat",
            "con .txt",
            "state.",
            "state ",
            "state:stream",
            "NUL.",
            "COM1 ",
        ] {
            assert!(!plain_component(name), "{name:?}");
        }
    }

    #[test]
    fn ordinary_components_are_kept() {
        for name in [
            "Ophio",
            "identity",
            ".identity-1.tmp",
            "console",
            "COM10",
            "Users",
        ] {
            assert!(plain_component(name), "{name:?}");
        }
    }
}
