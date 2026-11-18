// SPDX-License-Identifier: Apache-2.0
//! Driver counter identities and whole-adapter aggregation, independent of PDH.

use std::collections::{BTreeMap, BTreeSet};

/// The order and signed high half match DXGI's `LUID` fields.
pub(crate) type AdapterLuid = (u32, i32);
pub(crate) type GpuReadings = BTreeMap<AdapterLuid, (Option<f32>, Option<u64>)>;

#[derive(Debug)]
pub(crate) struct CounterSample<T> {
    pub(crate) instance: String,
    /// An invalid, missing or not-yet-calculable counter is not a zero.
    pub(crate) value: Option<T>,
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Engine {
    adapter: AdapterLuid,
    physical: u32,
    engine: u32,
    pid: u32,
}

fn decimal(text: &str) -> Option<u32> {
    (!text.is_empty() && text.bytes().all(|c| c.is_ascii_digit())).then(|| text.parse().ok())?
}

fn hex(text: &str) -> Option<u32> {
    let digits = text.strip_prefix("0x")?;
    (!digits.is_empty() && digits.len() <= 8 && digits.bytes().all(|c| c.is_ascii_hexdigit()))
        .then(|| u32::from_str_radix(digits, 16).ok())?
}

/// Counter instance names spell the *high* LUID half first, unlike DXGI's
/// field order. Preserve its bits, including negative signed high halves.
fn adapter<'a>(parts: &mut impl Iterator<Item = &'a str>) -> Option<AdapterLuid> {
    if parts.next()? != "luid" {
        return None;
    }
    let high = hex(parts.next()?)?;
    let low = hex(parts.next()?)?;
    Some((low, high as i32))
}

fn physical<'a>(parts: &mut impl Iterator<Item = &'a str>) -> Option<u32> {
    (parts.next()? == "phys").then_some(())?;
    decimal(parts.next()?)
}

fn engine(instance: &str) -> Option<Engine> {
    let mut parts = instance.split('_');
    (parts.next()? == "pid").then_some(())?;
    let pid = decimal(parts.next()?)?;
    let adapter = adapter(&mut parts)?;
    let physical = physical(&mut parts)?;
    (parts.next()? == "eng").then_some(())?;
    let engine = decimal(parts.next()?)?;
    (parts.next()? == "engtype").then_some(())?;
    // The display label is not the engine identity. Some labels contain '_'.
    let labels: Vec<_> = parts.collect();
    if labels.is_empty()
        || labels
            .iter()
            .any(|s| s.is_empty() || !s.bytes().all(|c| c.is_ascii_alphanumeric()))
    {
        return None;
    }
    Some(Engine {
        adapter,
        physical,
        engine,
        pid,
    })
}

fn memory_instance(instance: &str) -> Option<(AdapterLuid, u32)> {
    let mut parts = instance.split('_');
    let adapter = adapter(&mut parts)?;
    let physical = physical(&mut parts)?;
    parts.next().is_none().then_some((adapter, physical))
}

/// Sum process contributions to the same physical engine, then take the
/// busiest engine, as Windows does. Different engines may execute in parallel:
/// adding their percentages would double-count adapter time.
fn utilization(rows: &[CounterSample<f64>]) -> BTreeMap<AdapterLuid, Option<f32>> {
    let mut engines = BTreeMap::<(AdapterLuid, u32, u32), f64>::new();
    let mut seen = BTreeSet::new();
    let mut invalid = BTreeSet::new();
    for row in rows {
        let Some(key) = engine(&row.instance) else {
            // An unidentifiable row could be a missing contribution to any
            // adapter. Do not report a partial sample as an idle adapter.
            return BTreeMap::new();
        };
        let adapter = key.adapter;
        let total = engines
            .entry((adapter, key.physical, key.engine))
            .or_default();
        if !seen.insert(key) {
            invalid.insert(adapter);
        }
        match row.value {
            Some(value) if value.is_finite() && value >= 0.0 => {
                *total += value;
                if !total.is_finite() {
                    invalid.insert(adapter);
                }
            }
            _ => {
                invalid.insert(adapter);
            }
        }
    }
    let mut busiest = BTreeMap::<AdapterLuid, Option<f32>>::new();
    for ((adapter, _, _), total) in engines {
        if invalid.contains(&adapter) {
            busiest.insert(adapter, None);
        } else {
            let busy = busiest.entry(adapter).or_insert(Some(0.0));
            *busy = Some(busy.unwrap_or(0.0).max(total.min(100.0) as f32));
        }
    }
    busiest
}

/// Adapter-memory instances are whole-physical-adapter byte gauges, not
/// per-process deltas. Sum distinct physical nodes of a linked DXGI adapter.
fn dedicated_memory(rows: &[CounterSample<u64>]) -> BTreeMap<AdapterLuid, Option<u64>> {
    let mut totals = BTreeMap::<AdapterLuid, Option<u64>>::new();
    let mut seen = BTreeSet::new();
    for row in rows {
        let Some(key) = memory_instance(&row.instance) else {
            return BTreeMap::new();
        };
        let total = totals.entry(key.0).or_insert(Some(0));
        *total = if seen.insert(key) {
            total.and_then(|sum| sum.checked_add(row.value?))
        } else {
            None
        };
    }
    totals
}

pub(crate) fn readings(
    engines: Option<&[CounterSample<f64>]>,
    memory: Option<&[CounterSample<u64>]>,
    engine_ready: bool,
) -> GpuReadings {
    let mut out = GpuReadings::new();
    if engine_ready && let Some(engines) = engines {
        for (adapter, busy) in utilization(engines) {
            out.entry(adapter).or_default().0 = busy;
        }
    }
    if let Some(memory) = memory {
        for (adapter, used) in dedicated_memory(memory) {
            out.entry(adapter).or_default().1 = used;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: AdapterLuid = (0xabcd, -1);
    const B: AdapterLuid = (0x1234, 0);

    fn engine_row(pid: u32, physical: u32, engine: u32, value: Option<f64>) -> CounterSample<f64> {
        CounterSample {
            instance: format!(
                "pid_{pid}_luid_0xffffffff_0x0000abcd_phys_{physical}_eng_{engine}_engtype_3D"
            ),
            value,
        }
    }

    fn memory_row(physical: u32, value: Option<u64>) -> CounterSample<u64> {
        CounterSample {
            instance: format!("luid_0xffffffff_0x0000abcd_phys_{physical}"),
            value,
        }
    }

    #[test]
    fn matches_dxgi_luid_order_and_preserves_high_bits() {
        assert_eq!(
            engine(&engine_row(8, 0, 1, Some(2.0)).instance)
                .unwrap()
                .adapter,
            A
        );
        assert_eq!(
            memory_instance("luid_0x00000000_0x00001234_phys_0"),
            Some((B, 0))
        );
        let rows = [CounterSample {
            instance: "pid_1_luid_0x00000000_0x00001234_phys_0_eng_0_engtype_Copy".into(),
            value: Some(5.0),
        }];
        let result = readings(Some(&rows), None, true);
        assert_eq!(result[&B], (Some(5.0), None));
        assert!(!result.contains_key(&A));
    }

    #[test]
    fn sums_processes_on_one_engine_but_not_different_engines_or_nodes() {
        let rows = [
            engine_row(1, 0, 0, Some(20.0)),
            engine_row(2, 0, 0, Some(30.0)),
            engine_row(1, 0, 1, Some(40.0)),
            engine_row(1, 1, 0, Some(45.0)),
        ];
        assert_eq!(readings(Some(&rows), None, true)[&A].0, Some(50.0));
    }

    #[test]
    fn caps_oversubscribed_engine_and_accepts_observed_idle() {
        let rows = [
            engine_row(1, 0, 0, Some(70.0)),
            engine_row(2, 0, 0, Some(60.0)),
        ];
        assert_eq!(readings(Some(&rows), None, true)[&A].0, Some(100.0));
        assert_eq!(
            readings(Some(&[engine_row(1, 0, 0, Some(0.0))]), None, true)[&A].0,
            Some(0.0)
        );
    }

    #[test]
    fn first_missing_and_invalid_samples_are_not_idle() {
        assert!(readings(None, None, true).is_empty());
        assert!(readings(Some(&[]), Some(&[]), true).is_empty());
        assert!(readings(Some(&[engine_row(1, 0, 0, Some(0.0))]), None, false).is_empty());
        for invalid in [None, Some(f64::NAN), Some(f64::INFINITY), Some(-1.0)] {
            let rows = [engine_row(1, 0, 0, Some(0.0)), engine_row(2, 0, 1, invalid)];
            assert_eq!(readings(Some(&rows), None, true)[&A].0, None);
        }
    }

    #[test]
    fn incomplete_engine_does_not_hide_behind_valid_other_engine() {
        let rows = [engine_row(1, 0, 0, Some(10.0)), engine_row(2, 0, 0, None)];
        assert_eq!(readings(Some(&rows), None, true)[&A].0, None);
        let duplicate = [
            engine_row(1, 0, 0, Some(10.0)),
            engine_row(1, 0, 0, Some(10.0)),
        ];
        assert_eq!(readings(Some(&duplicate), None, true)[&A].0, None);
    }

    #[test]
    fn malformed_or_missing_instance_identity_invalidates_that_counter_only() {
        for instance in [
            "",
            "_Total",
            "pid_1",
            "pid_-1_luid_0x0_0x1_phys_0_eng_0_engtype_3D",
            "pid_1_luid_0x100000000_0x1_phys_0_eng_0_engtype_3D",
            "pid_1_luid_0xzz_0x1_phys_0_eng_0_engtype_3D",
            "pid_1_luid_0x0_0x1_phys_0_eng_0",
            "pid_1_luid_0x0_0x1_phys_0_eng_0_engtype_",
        ] {
            let rows = [
                engine_row(1, 0, 0, Some(0.0)),
                CounterSample {
                    instance: instance.into(),
                    value: Some(0.0),
                },
            ];
            let memory = [memory_row(0, Some(99))];
            assert_eq!(
                readings(Some(&rows), Some(&memory), true)[&A],
                (None, Some(99)),
                "{instance}"
            );
        }
        let rows = [
            memory_row(0, Some(0)),
            CounterSample {
                instance: "luid_0x0_0x1_phys_0_extra".into(),
                value: Some(0),
            },
        ];
        assert!(readings(None, Some(&rows), true).is_empty());
    }

    #[test]
    fn memory_is_a_byte_gauge_available_even_before_second_engine_sample() {
        let rows = [memory_row(0, Some(1_234_567_890)), memory_row(1, Some(42))];
        assert_eq!(
            readings(None, Some(&rows), false)[&A],
            (None, Some(1_234_567_932))
        );
        // Gauges can go down without underflow or delta calculations.
        assert_eq!(
            readings(None, Some(&[memory_row(0, Some(3))]), false)[&A].1,
            Some(3)
        );
        assert_eq!(
            readings(None, Some(&[memory_row(0, Some(0))]), false)[&A].1,
            Some(0)
        );
    }

    #[test]
    fn incomplete_duplicate_and_overflowing_memory_is_absent() {
        for rows in [
            vec![memory_row(0, Some(5)), memory_row(1, None)],
            vec![memory_row(0, Some(5)), memory_row(0, Some(5))],
            vec![memory_row(0, Some(u64::MAX)), memory_row(1, Some(1))],
        ] {
            assert_eq!(readings(None, Some(&rows), true)[&A].1, None);
        }
    }
}
