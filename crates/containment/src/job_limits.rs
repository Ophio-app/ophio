// SPDX-License-Identifier: Apache-2.0
//! Translate the shared CPU allowance (one CPU's milliseconds per second) to
//! a Job hard cap (hundredths of a percent of all active logical processors).
//! This is a rate, not a lifetime CPU-time budget. Rounding always tightens it.
#[cfg(test)]
use crate::ResourceLimits;
use ports::HarnessError;

#[cfg(test)]
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct JobLimits {
    pub memory_bytes: usize,
    pub processes: u32,
    pub cpu_rate: u32,
}

#[cfg(test)]
impl JobLimits {
    pub(crate) fn new(limits: ResourceLimits, processors: u32) -> Result<Self, HarnessError> {
        if limits.memory_bytes == 0 || limits.cpu_millis_per_second == 0 || limits.processes == 0 {
            return Err(unsupported("Resource limits must be positive"));
        }
        if processors == 0 {
            return Err(unsupported("Windows could not count active processors"));
        }
        let memory_bytes = usize::try_from(limits.memory_bytes).map_err(|_| {
            unsupported("The memory limit exceeds this Windows process's address size")
        })?;
        // At most the machine's entire capacity; never round up above the
        // requested number of CPU milliseconds. The smallest Job rate is 1.
        let cpu_rate = (u64::from(limits.cpu_millis_per_second) * 10 / u64::from(processors))
            .min(10_000) as u32;
        if cpu_rate == 0 {
            return Err(unsupported(
                "The CPU allowance is below this computer's smallest Windows Job hard cap",
            ));
        }
        Ok(Self {
            memory_bytes,
            processes: limits.processes,
            cpu_rate,
        })
    }
}

pub(crate) fn external_adoption(limited: bool) -> Result<(), HarnessError> {
    if limited {
        Err(unsupported(
            "A resource-limited Job cannot adopt an externally created process; use suspended-first spawn or a creation-time Job attribute",
        ))
    } else {
        Ok(())
    }
}

fn unsupported(message: &str) -> HarnessError {
    HarnessError::Unsupported(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits(cpu: u32) -> ResourceLimits {
        ResourceLimits {
            memory_bytes: 128 * 1024 * 1024,
            cpu_millis_per_second: cpu,
            processes: 8,
        }
    }

    #[test]
    fn cpu_rate_counts_all_processors_and_is_not_a_lifetime_budget() {
        assert_eq!(JobLimits::new(limits(1000), 1).unwrap().cpu_rate, 10_000);
        assert_eq!(JobLimits::new(limits(1000), 8).unwrap().cpu_rate, 1250);
        assert_eq!(JobLimits::new(limits(250), 8).unwrap().cpu_rate, 312);
        assert_eq!(JobLimits::new(limits(2000), 8).unwrap().cpu_rate, 2500);
        assert_eq!(
            JobLimits::new(limits(u32::MAX), 8).unwrap().cpu_rate,
            10_000
        );
    }

    #[test]
    fn rounding_never_allows_more_than_requested() {
        for processors in [1, 2, 3, 8, 64, 128, 256] {
            for cpu in [1, 7, 50, 250, 1000, 4000, u32::MAX] {
                if let Ok(translated) = JobLimits::new(limits(cpu), processors) {
                    assert!(translated.cpu_rate > 0 && translated.cpu_rate <= 10_000);
                    assert!(
                        u64::from(translated.cpu_rate) * u64::from(processors)
                            <= u64::from(cpu) * 10
                    );
                } else {
                    assert!(u64::from(cpu) * 10 < u64::from(processors));
                }
            }
        }
    }

    #[test]
    fn missing_processor_count_and_unrepresentable_rate_fail_closed() {
        assert!(JobLimits::new(limits(1000), 0).is_err());
        assert!(JobLimits::new(limits(1), 64).is_err());
        assert_eq!(JobLimits::new(limits(7), 64).unwrap().cpu_rate, 1);
    }

    #[test]
    fn every_limit_must_be_positive() {
        let mut value = limits(1000);
        value.memory_bytes = 0;
        assert!(JobLimits::new(value, 8).is_err());
        value = limits(0);
        assert!(JobLimits::new(value, 8).is_err());
        value = limits(1000);
        value.processes = 0;
        assert!(JobLimits::new(value, 8).is_err());
    }

    #[test]
    fn external_adoption_cannot_evade_a_limited_jobs_memory_accounting() {
        assert!(matches!(
            external_adoption(true),
            Err(HarnessError::Unsupported(_))
        ));
        assert!(external_adoption(false).is_ok());
    }

    #[test]
    fn memory_and_process_counts_remain_exact() {
        let translated = JobLimits::new(limits(1000), 8).unwrap();
        assert_eq!(translated.memory_bytes, 128 * 1024 * 1024);
        assert_eq!(translated.processes, 8);
        if usize::BITS < 64 {
            let mut value = limits(1000);
            value.memory_bytes = u64::MAX;
            assert!(JobLimits::new(value, 8).is_err());
        }
    }
}
