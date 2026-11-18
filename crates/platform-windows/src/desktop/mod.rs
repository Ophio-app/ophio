// SPDX-License-Identifier: Apache-2.0
//! Watching and controlling this Windows desktop. Which input may go where, in
//! what order, and how it is released are shared with the Mac in
//! `platform-desktop`; only what talks to Windows is here. Construct only
//! after desktop sharing has been explicitly enabled by the owner.
#[cfg(windows)]
#[allow(unsafe_code)]
mod capture;
#[cfg(windows)]
mod clipboard;
mod keys;
#[cfg(windows)]
#[allow(unsafe_code)]
mod native;

#[cfg(windows)]
use async_trait::async_trait;
#[cfg(windows)]
use platform_desktop::{
    Capture, Driver, ManagedDesktop,
    geometry::{self, Layout, Monitor},
    input::{self, Injector},
};
#[cfg(windows)]
use ports::{Clock, SystemClock, desktop::*};
#[cfg(windows)]
use protocol::{
    capability::{Availability, InputScope},
    control::{DesktopLockState, TargetGeometry},
};
#[cfg(windows)]
use std::{
    collections::BTreeMap,
    sync::Arc,
    time::{Duration, Instant},
};

/// Opens this computer's desktop: opts this process into per-monitor v2 DPI
/// awareness and refuses an incompatible existing DPI policy or an elevated
/// process.
#[cfg(windows)]
pub fn open() -> Result<ManagedDesktop, PlatformError> {
    Ok(ManagedDesktop::with_driver(Arc::new(
        native::NativeDriver::new()?,
    )))
}

#[cfg(windows)]
pub(crate) fn capabilities(availability: Availability) -> DesktopCapabilities {
    DesktopCapabilities {
        backend: "windows".into(),
        capture: availability.clone(),
        input: availability.clone(),
        input_scope: InputScope::FocusedTarget,
        lock_session: availability,
        unlock_session: Availability::Unsupported {
            reason: "Windows lets only its own sign-in screen unlock it".into(),
        },
    }
}
