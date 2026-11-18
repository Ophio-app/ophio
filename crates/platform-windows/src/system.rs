// SPDX-License-Identifier: Apache-2.0
//! How busy the computer is, as Windows counts it: processor time, memory,
//! fixed drives, network counters, pagefiles, battery, graphics adapters,
//! uptime and the processes this user may look at. Unsupported sensors and
//! unavailable driver graphics counters are left out rather than reported as zero.
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

/// Native fixed-size strings may include padding and trailing spaces.
#[cfg(any(windows, test))]
pub(crate) fn native_text(wide: &[u16]) -> Option<String> {
    let end = wide.iter().position(|&c| c == 0).unwrap_or(wide.len());
    let text = String::from_utf16_lossy(&wide[..end]).trim().to_owned();
    (!text.is_empty()).then_some(text)
}

#[cfg(any(windows, test))]
pub(crate) fn battery_reading(flags: u8, percent: u8) -> Option<ports::system::BatteryReading> {
    // 128 means no battery; 255 means unknown, not all flags set.
    if flags == 255 || flags & 128 != 0 || percent > 100 {
        return None;
    }
    Some(ports::system::BatteryReading {
        percent: f32::from(percent),
        charging: flags & 8 != 0,
    })
}

#[cfg(any(windows, test))]
pub(crate) fn pagefile_reading(
    total: u64,
    used: u64,
    page_size: u64,
) -> Option<ports::system::SwapReading> {
    if page_size == 0 || used > total {
        return None;
    }
    Some(ports::system::SwapReading {
        total: total.checked_mul(page_size)?,
        free: (total - used).checked_mul(page_size)?,
        swapped_in: None,
        swapped_out: None,
    })
}

/// Available RAM includes standby cache. Free and zeroed pages, unlike
/// `GlobalMemoryStatusEx`'s available count, are actually unused RAM.
#[cfg(any(windows, test))]
pub(crate) fn memory_reading(
    total: u64,
    available: u64,
    zero_pages: u64,
    free_pages: u64,
    page_size: u64,
) -> Option<ports::system::MemoryReading> {
    if total == 0 || page_size == 0 || available > total {
        return None;
    }
    let free = zero_pages.checked_add(free_pages)?.checked_mul(page_size)?;
    if free > available {
        return None;
    }
    Some(ports::system::MemoryReading {
        total,
        available,
        free,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn available_memory_is_not_mistaken_for_free_ram_or_zero_cache() {
        let reading = memory_reading(1024, 512, 1, 1, 64).unwrap();
        assert_eq!(reading.free, 128);
        assert_eq!(reading.available - reading.free, 384);
        assert_eq!(memory_reading(1024, 512, 0, 0, 64).unwrap().free, 0);
        assert_eq!(memory_reading(1024, 512, 1, 1, 0), None);
        assert_eq!(memory_reading(1024, 512, 5, 5, 64), None);
        assert_eq!(memory_reading(1024, 2048, 0, 0, 64), None);
        assert_eq!(memory_reading(1024, 512, u64::MAX, 1, 64), None);
        assert_eq!(memory_reading(1024, 512, u64::MAX / 2, 0, 64), None);
    }

    #[test]
    fn native_strings_stop_at_nul_and_reject_blank_values() {
        let wide = |s: &str| s.encode_utf16().collect::<Vec<_>>();
        assert_eq!(
            native_text(&wide("  Processor  \0ignored")),
            Some("Processor".into())
        );
        assert_eq!(native_text(&wide("Graphics α")), Some("Graphics α".into()));
        assert_eq!(native_text(&wide(" \0")), None);
        assert_eq!(native_text(&[]), None);
    }

    #[test]
    fn battery_unknown_and_absent_are_not_empty_batteries() {
        assert_eq!(battery_reading(255, 50), None);
        assert_eq!(battery_reading(128, 0), None);
        assert_eq!(battery_reading(8, 255), None);
        assert_eq!(battery_reading(0, 101), None);
        let empty = battery_reading(4, 0).unwrap();
        assert_eq!(empty.percent, 0.0);
        assert!(!empty.charging);
        let full = battery_reading(8, 100).unwrap();
        assert_eq!(full.percent, 100.0);
        assert!(full.charging);
    }

    #[test]
    fn pagefile_pages_are_bytes_not_commit_limit_or_transfer_counters() {
        let reading = pagefile_reading(100, 25, 4096).unwrap();
        assert_eq!(reading.total, 409600);
        assert_eq!(reading.free, 307200);
        assert_eq!(reading.swapped_in, None);
        assert_eq!(reading.swapped_out, None);
        assert_eq!(pagefile_reading(0, 0, 4096).unwrap().total, 0);
        assert_eq!(pagefile_reading(10, 11, 4096), None);
        assert_eq!(pagefile_reading(10, 0, 0), None);
        assert_eq!(pagefile_reading(u64::MAX, 0, 4096), None);
    }

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
