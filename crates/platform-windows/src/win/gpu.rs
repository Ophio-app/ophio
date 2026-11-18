// SPDX-License-Identifier: Apache-2.0
//! Optional whole-adapter GPU readings from the driver's native PDH counters.

use crate::gpu::{CounterSample, GpuReadings, readings};
use std::sync::Mutex;
use windows::{
    Win32::System::Performance::{
        PDH_CSTATUS_NEW_DATA, PDH_CSTATUS_VALID_DATA, PDH_FMT, PDH_FMT_COUNTERVALUE,
        PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE, PDH_FMT_LARGE, PDH_HCOUNTER, PDH_HQUERY,
        PDH_MORE_DATA, PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData,
        PdhGetFormattedCounterArrayW, PdhOpenQueryW,
    },
    core::{PCWSTR, PWSTR, w},
};

const ENGINE_PATH: PCWSTR = w!("\\GPU Engine(*)\\Utilization Percentage");
const MEMORY_PATH: PCWSTR = w!("\\GPU Adapter Memory(*)\\Dedicated Usage");
// Driver instance sets can grow between the size and data calls. Bound both
// allocation and retries, rather than blocking a system reading indefinitely.
const MAX_ARRAY_BYTES: usize = 16 * 1024 * 1024;
const ARRAY_ATTEMPTS: usize = 3;

#[derive(Debug, Default)]
pub(crate) struct GpuCounters {
    state: Mutex<State>,
}

#[derive(Debug, Default)]
struct State {
    engines: Option<Query>,
    memory: Option<Query>,
}

impl GpuCounters {
    /// Keys are DXGI `(AdapterLuid.LowPart, AdapterLuid.HighPart)`; values are
    /// `(busy_percent, dedicated_bytes_used)`. Missing/denied driver counters,
    /// first utilization samples and invalid data remain absent. Never sleeps.
    pub(crate) fn read(&self) -> GpuReadings {
        let Ok(mut state) = self.state.lock() else {
            // A poisoned sampling state is not evidence that the GPU is idle.
            return GpuReadings::new();
        };
        let (engines, ready) = collect(
            &mut state.engines,
            ENGINE_PATH,
            PDH_FMT_DOUBLE,
            true,
            utilization_value,
        );
        let (memory, _) = collect(
            &mut state.memory,
            MEMORY_PATH,
            PDH_FMT_LARGE,
            false,
            memory_value,
        );
        readings(engines.as_deref(), memory.as_deref(), ready)
    }
}

/// Each query owns one counter. Separate queries let missing adapter-memory
/// support coexist with working engine counters (and vice versa).
#[derive(Debug)]
struct Query {
    handle: PDH_HQUERY,
    counter: PDH_HCOUNTER,
    primed: bool,
}

// SAFETY: PDH query handles are not thread-affine. They are opaque values, not
// dereferenced Rust pointers, and all collection/formatting/closing is serialized
// by GpuCounters' mutex. The owned counter never escapes its owned query.
unsafe impl Send for Query {}

impl Query {
    fn open(path: PCWSTR) -> Option<Self> {
        let mut handle = PDH_HQUERY::default();
        // SAFETY: null data source selects current local counters; the output
        // handle is initialized by PDH. No log file or remote source is opened.
        if unsafe { PdhOpenQueryW(PCWSTR::null(), 0, &mut handle) } != 0 || handle.is_invalid() {
            return None;
        }
        let mut query = Self {
            handle,
            counter: PDH_HCOUNTER::default(),
            primed: false,
        };
        // SAFETY: the query is live and path is a static, terminated English
        // counter path. PDH translates counter names on localized Windows.
        if unsafe { PdhAddEnglishCounterW(handle, path, 0, &mut query.counter) } != 0
            || query.counter.is_invalid()
        {
            return None; // Query::drop closes the partially initialized query.
        }
        Some(query)
    }
}

impl Drop for Query {
    fn drop(&mut self) {
        // SAFETY: this is the sole owner. Closing also releases its counter;
        // neither handle escapes the mutex-protected state.
        unsafe { PdhCloseQuery(self.handle) };
    }
}

fn collect<T>(
    slot: &mut Option<Query>,
    path: PCWSTR,
    format: PDH_FMT,
    rate: bool,
    decode: fn(PDH_FMT_COUNTERVALUE) -> Option<T>,
) -> (Option<Vec<CounterSample<T>>>, bool) {
    if slot.is_none() {
        *slot = Query::open(path);
    }
    let Some(query) = slot else {
        return (None, false);
    };
    // SAFETY: the query and its counter are owned and exclusively borrowed.
    if unsafe { PdhCollectQueryData(query.handle) } != 0 {
        // Reopen on the next read, so unavailable/restarted providers recover
        // without keeping a stale utilization baseline.
        *slot = None;
        return (None, false);
    }
    let ready = query.primed;
    query.primed = true;
    if rate && !ready {
        // Rate counters need two samples. PDH may give a numeric zero for a
        // first sample; deliberately do not expose it as measured idle time.
        return (None, false);
    }
    let values = formatted(query.counter, format, decode);
    if values.is_none() {
        *slot = None;
    }
    (values, ready)
}

fn valid(value: &PDH_FMT_COUNTERVALUE) -> bool {
    matches!(value.CStatus, PDH_CSTATUS_VALID_DATA | PDH_CSTATUS_NEW_DATA)
}

fn utilization_value(value: PDH_FMT_COUNTERVALUE) -> Option<f64> {
    if !valid(&value) {
        return None;
    }
    // SAFETY: this decoder is used only with PDH_FMT_DOUBLE.
    let busy = unsafe { value.Anonymous.doubleValue };
    (busy.is_finite() && busy >= 0.0).then_some(busy)
}

fn memory_value(value: PDH_FMT_COUNTERVALUE) -> Option<u64> {
    if !valid(&value) {
        return None;
    }
    // SAFETY: this decoder is used only with PDH_FMT_LARGE. This is a byte
    // gauge, not a delta or this process's DXGI memory budget.
    u64::try_from(unsafe { value.Anonymous.largeValue }).ok()
}

fn formatted<T>(
    counter: PDH_HCOUNTER,
    format: PDH_FMT,
    decode: fn(PDH_FMT_COUNTERVALUE) -> Option<T>,
) -> Option<Vec<CounterSample<T>>> {
    let (mut bytes, mut count) = (0, 0);
    // SAFETY: a null output requests the required byte count only.
    if unsafe { PdhGetFormattedCounterArrayW(counter, format, &mut bytes, &mut count, None) }
        != PDH_MORE_DATA
    {
        return None;
    }
    for _ in 0..ARRAY_ATTEMPTS {
        let capacity = bytes as usize;
        if capacity == 0 || capacity > MAX_ARRAY_BYTES {
            return None;
        }
        // Allocate in item units for correct native alignment. PDH places
        // item headers followed by UTF-16 names in this same allocation.
        let slots = capacity.div_ceil(size_of::<PDH_FMT_COUNTERVALUE_ITEM_W>());
        let mut buffer = Vec::<PDH_FMT_COUNTERVALUE_ITEM_W>::new();
        buffer.try_reserve_exact(slots).ok()?;
        buffer.resize_with(slots, Default::default);
        // SAFETY: all-zero bit patterns are valid for these generated structs
        // (including their pointer/union fields). Initialize padding too, so a
        // bounded string scan never reads an uninitialized padding byte.
        unsafe {
            std::ptr::write_bytes(
                buffer.as_mut_ptr().cast::<u8>(),
                0,
                size_of_val(buffer.as_slice()),
            );
        }
        // SAFETY: the aligned writable allocation covers `bytes`; PDH writes
        // only within that bound. No slice of untrusted item count is formed.
        let status = unsafe {
            PdhGetFormattedCounterArrayW(
                counter,
                format,
                &mut bytes,
                &mut count,
                Some(buffer.as_mut_ptr()),
            )
        };
        if status == PDH_MORE_DATA {
            continue;
        }
        if status != 0 || bytes as usize > capacity || count == 0 {
            return None;
        }
        let used = bytes as usize;
        let headers = (count as usize).checked_mul(size_of::<PDH_FMT_COUNTERVALUE_ITEM_W>())?;
        if headers > used || count as usize > buffer.len() {
            return None;
        }
        let mut rows = Vec::new();
        rows.try_reserve_exact(count as usize).ok()?;
        for item in &buffer[..count as usize] {
            rows.push(CounterSample {
                instance: bounded_name(&buffer, used, headers, item.szName)?,
                value: decode(item.FmtValue),
            });
        }
        return Some(rows);
    }
    None
}

/// Validate every native string pointer and terminator against the returned
/// allocation. `PWSTR::to_string` would scan unbounded native memory.
fn bounded_name(
    buffer: &[PDH_FMT_COUNTERVALUE_ITEM_W],
    used: usize,
    headers: usize,
    name: PWSTR,
) -> Option<String> {
    if used > size_of_val(buffer) || headers > used {
        return None;
    }
    let start = name.0.addr().checked_sub(buffer.as_ptr().addr())?;
    if start < headers || start >= used || start % align_of::<u16>() != 0 {
        return None;
    }
    // SAFETY: start and the length are bounded above by this allocation's
    // initialized bytes, excluding item headers; alignment is checked above.
    // Reconstruct from the allocation pointer to preserve its provenance.
    let text = unsafe {
        std::slice::from_raw_parts(
            buffer.as_ptr().cast::<u8>().add(start).cast::<u16>(),
            (used - start) / size_of::<u16>(),
        )
    };
    let end = text.iter().position(|&unit| unit == 0)?;
    if end == 0 {
        return None;
    }
    String::from_utf16(&text[..end]).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::System::Performance::PDH_FMT_COUNTERVALUE_0;

    #[test]
    fn invalid_counter_status_never_becomes_numeric_zero() {
        let value = PDH_FMT_COUNTERVALUE {
            CStatus: 0xc0000bba,
            Anonymous: PDH_FMT_COUNTERVALUE_0 { doubleValue: 0.0 },
        };
        assert_eq!(utilization_value(value), None);
        assert_eq!(memory_value(value), None);
        for status in [PDH_CSTATUS_VALID_DATA, PDH_CSTATUS_NEW_DATA] {
            let value = PDH_FMT_COUNTERVALUE {
                CStatus: status,
                Anonymous: PDH_FMT_COUNTERVALUE_0 { doubleValue: 12.5 },
            };
            assert_eq!(utilization_value(value), Some(12.5));
            let value = PDH_FMT_COUNTERVALUE {
                CStatus: status,
                Anonymous: PDH_FMT_COUNTERVALUE_0 {
                    largeValue: 123_456_789,
                },
            };
            assert_eq!(memory_value(value), Some(123_456_789));
        }
    }

    #[test]
    fn bounded_utf16_rejects_outside_headers_unterminated_and_invalid_text() {
        let mut buffer = vec![PDH_FMT_COUNTERVALUE_ITEM_W::default(); 4];
        let used = size_of_val(buffer.as_slice());
        let headers = size_of::<PDH_FMT_COUNTERVALUE_ITEM_W>();
        // SAFETY: initializes the complete allocation, including padding.
        unsafe { std::ptr::write_bytes(buffer.as_mut_ptr().cast::<u8>(), 0, used) };
        let base = buffer.as_mut_ptr().cast::<u8>();
        // SAFETY: the header occupies one of four allocated items. The
        // remaining space is aligned UTF-16 storage within this allocation.
        let name = unsafe { base.add(headers).cast::<u16>() };
        // SAFETY: two writable UTF-16 units fit after the header.
        unsafe {
            name.write(0x0041);
            name.add(1).write(0);
        }
        assert_eq!(
            bounded_name(&buffer, used, headers, PWSTR(name)),
            Some("A".into())
        );
        assert_eq!(bounded_name(&buffer, used, headers, PWSTR::null()), None);
        assert_eq!(
            bounded_name(&buffer, used, headers, PWSTR(base.cast())),
            None
        );
        assert_eq!(
            bounded_name(
                &buffer,
                used,
                headers,
                PWSTR(base.wrapping_add(used).cast())
            ),
            None
        );
        assert_eq!(
            bounded_name(
                &buffer,
                used,
                headers,
                PWSTR(name.cast::<u8>().wrapping_add(1).cast())
            ),
            None
        );
        assert_eq!(
            bounded_name(&buffer, headers + 2, headers, PWSTR(name)),
            None
        );
        // SAFETY: same initialized writable name storage, now an unpaired
        // high surrogate followed by its terminator.
        unsafe { name.write(0xd800) };
        assert_eq!(bounded_name(&buffer, used, headers, PWSTR(name)), None);
    }

    #[test]
    fn native_counters_are_optional_and_first_utilization_is_absent() {
        // Read-only local counters: no desktop, workload or permissions
        // changes. Drivers may provide neither counter on this machine.
        let counters = GpuCounters::default();
        let first = counters.read();
        assert!(first.values().all(|(busy, _)| busy.is_none()));
        let next = counters.read();
        assert!(
            next.values()
                .all(|(busy, _)| busy.is_none_or(|busy| (0.0..=100.0).contains(&busy)))
        );
    }

    #[test]
    fn poisoned_state_returns_no_reading_without_native_access() {
        let counters = GpuCounters::default();
        let _ = std::panic::catch_unwind(|| {
            let _guard = counters.state.lock().unwrap();
            panic!("synthetic sampler failure");
        });
        assert!(counters.read().is_empty());
    }
}
