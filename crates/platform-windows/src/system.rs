// SPDX-License-Identifier: Apache-2.0
//! How busy the computer is, as Windows counts it: processor time, memory,
//! fixed drives, uptime and the processes this user may look at. Swap,
//! network, temperatures and graphics aren't read here, so Control leaves
//! them out.
#[cfg(windows)]
pub use crate::win::system::WindowsSystem;

/// Each drive's root from `GetLogicalDrives`' mask, in which bit 0 is `A:`.
#[cfg(any(windows, test))]
pub(crate) fn drive_roots(mask: u32) -> Vec<String> {
    (0..26u8)
        .filter(|i| mask & (1 << i) != 0)
        .map(|i| format!("{}:\\", char::from(b'A' + i)))
        .collect()
}

/// A process's name from its executable's file name, as Linux names one:
/// `Code.exe` is `Code`.
#[cfg(any(windows, test))]
pub(crate) fn process_name(exe: &[u16]) -> String {
    let end = exe.iter().position(|&c| c == 0).unwrap_or(exe.len());
    let name = String::from_utf16_lossy(&exe[..end]);
    match name.len().checked_sub(4) {
        Some(stem) if name.is_char_boundary(stem) && name[stem..].eq_ignore_ascii_case(".exe") => {
            name[..stem].to_string()
        }
        _ => name,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_each_drive_in_the_mask() {
        assert_eq!(drive_roots(0), Vec::<String>::new());
        assert_eq!(drive_roots(0b1100), vec!["C:\\", "D:\\"]);
        assert_eq!(drive_roots(1 | 1 << 25 | 1 << 31), vec!["A:\\", "Z:\\"]);
    }

    #[test]
    fn names_a_process_without_its_extension() {
        let wide = |s: &str| s.encode_utf16().chain([0, 0x41]).collect::<Vec<_>>();
        assert_eq!(process_name(&wide("Code.exe")), "Code");
        assert_eq!(process_name(&wide("SYSTEM.EXE")), "SYSTEM");
        assert_eq!(process_name(&wide("System")), "System");
        assert_eq!(process_name(&wide(".exe")), "");
        assert_eq!(process_name(&wide("é.exe")), "é");
        assert_eq!(process_name(&wide("ab")), "ab");
        assert_eq!(
            process_name(&"no-nul".encode_utf16().collect::<Vec<_>>()),
            "no-nul"
        );
    }
}
