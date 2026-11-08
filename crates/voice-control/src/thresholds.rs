// SPDX-License-Identifier: Apache-2.0
//! Reviewed policy thresholds for the pinned Jev model.
use std::time::Duration;
pub const IS_COMMAND: f64 = 0.5;
pub const INTENT: f64 = 0.55;
pub const COMPLETE: f64 = 0.6;
pub const TARGET_CONFIDENCE: f64 = 0.45;
pub const TARGET_PROBABILITY: f64 = 0.35;
pub const DESTRUCTIVE: f64 = 0.5;
/// For a key or shortcut. Jev can't see what a desktop binds its shortcuts
/// to, and leans consequential when unsure (0.78 for Super+Enter, 0.99 for
/// Alt+F4 on 2026-11-07); the keys known to submit, close or delete are
/// confirmed whatever Jev says (`keys::risky`).
pub const DESTRUCTIVE_KEY: f64 = 0.9;
pub const CORRECTION: f64 = 0.6;
pub const SILENCE: Duration = Duration::from_millis(900);
pub const ACTION_INTERVAL: Duration = Duration::from_millis(500);
pub const IDLE: Duration = Duration::from_secs(600);
pub const ASSESS_TIMEOUT: Duration = Duration::from_secs(8);
pub const MAX_IN_FLIGHT: usize = 2;
pub const STEP_LIMIT: usize = 100;
pub const STALL_LIMIT: usize = 3;
