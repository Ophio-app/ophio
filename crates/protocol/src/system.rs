// SPDX-License-Identifier: Apache-2.0
//! How busy the computer is, for Control's System section. Read while
//! someone looks; nothing is kept between companion restarts.

use serde::{Deserialize, Serialize};

use crate::time::Timestamp;

/// Bytes are whole numbers; percentages run 0–100 and rates are per second.
/// A part the computer can't read is absent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct SystemStats {
    pub at: Timestamp,
    pub cpu: Option<CpuStats>,
    pub memory: Option<MemoryStats>,
    pub swap: Option<SwapStats>,
    /// Average runnable processes over 1, 5 and 15 minutes.
    pub load: Option<Vec<f64>>,
    #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
    pub uptime_s: Option<u64>,
    pub disks: Vec<DiskStats>,
    pub network: Option<NetworkStats>,
    /// The processor's own first.
    pub temperatures: Vec<Temperature>,
    pub gpus: Vec<GpuStats>,
    pub battery: Option<Battery>,
    /// The busiest few, by processor then memory.
    pub processes: Vec<ProcessStats>,
    /// Earlier samples, oldest first, about [`SYSTEM_SAMPLE_MS`] apart, ending
    /// with this one. A gap in looking starts it again.
    pub history: Vec<SystemSample>,
}

/// How often the phone asks while the System section is on screen.
pub const SYSTEM_SAMPLE_MS: u32 = 2_000;
/// How many samples [`SystemStats::history`] keeps.
pub const SYSTEM_HISTORY: usize = 90;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct CpuStats {
    pub usage: f32,
    /// Each processor's usage, in the system's order.
    pub cores: Vec<f32>,
    pub model: Option<String>,
}

/// `used + cached + free = total`. Cached is what the system holds for
/// speed and gives back when programs need it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct MemoryStats {
    #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
    pub total: u64,
    #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
    pub used: u64,
    #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
    pub cached: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct SwapStats {
    #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
    pub total: u64,
    #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
    pub used: u64,
    /// Bytes a second moved back into memory and out to swap; absent where
    /// the system doesn't count them.
    pub in_per_s: Option<f64>,
    pub out_per_s: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct DiskStats {
    pub mount: String,
    #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
    pub total: u64,
    #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
    pub used: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct NetworkStats {
    /// Bytes a second, all interfaces together.
    pub received_per_s: f64,
    pub sent_per_s: f64,
    pub interfaces: Vec<InterfaceStats>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct InterfaceStats {
    pub name: String,
    pub received_per_s: f64,
    pub sent_per_s: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct Temperature {
    pub label: String,
    pub celsius: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct GpuStats {
    pub name: String,
    pub usage: Option<f32>,
    #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
    pub memory_used: Option<u64>,
    #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
    pub memory_total: Option<u64>,
    pub celsius: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct Battery {
    pub percent: f32,
    pub charging: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct ProcessStats {
    pub pid: u32,
    /// The program's short name; never its command line.
    pub name: String,
    /// Share of the whole computer's processors.
    pub cpu: f32,
    #[schemars(range(max = crate::MAX_SAFE_INTEGER))]
    pub memory: u64,
}

/// One moment in the history: what the graphs draw.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, ts_rs::TS)]
#[ts(export)]
pub struct SystemSample {
    pub at: Timestamp,
    pub cpu: Option<f32>,
    /// Percent of memory in use, cache not counted.
    pub memory: Option<f32>,
    pub swap: Option<f32>,
    pub received_per_s: Option<f64>,
    pub sent_per_s: Option<f64>,
}
