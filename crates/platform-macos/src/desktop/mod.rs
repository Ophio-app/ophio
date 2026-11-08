// SPDX-License-Identifier: Apache-2.0
//! Watching and controlling this Mac's screen. Which input may go where, in what
//! order, and how it is released are the same as on Windows and shared with it;
//! only what talks to the system is here. Construct only after desktop sharing
//! has been explicitly enabled by the owner.
//!
//! Positions and sizes are in the system's points, which it takes for input
//! whatever a screen's scale, and frames are captured at that size.
mod capture;
pub mod input;
pub(crate) mod native;

use platform_windows::desktop::{
    Capture, Driver, WindowsDesktop, geometry::Monitor, input::Injector,
};
use ports::{
    Clock,
    desktop::{DesktopCapabilities, PlatformError},
};
use protocol::{
    capability::{Availability, InputScope},
    control::DesktopLockState,
};
use std::sync::Arc;

pub use native::{ask_for_permissions, permissions};

/// Opens this Mac's desktop. It works without permissions, and says what is
/// missing, so the person can be told where to give them.
pub fn open(clock: Arc<dyn Clock>) -> WindowsDesktop {
    WindowsDesktop::with_driver(Arc::new(MacDriver)).with_clock(clock)
}

#[derive(Debug)]
struct MacDriver;

const SCREEN_RECORDING: &str = "Give Øphio permission to record the screen, in System Settings › Privacy & Security › Screen & System Audio Recording, then start Øphio again.";
const ACCESSIBILITY: &str = "Give Øphio permission to control the computer, in System Settings › Privacy & Security › Accessibility, then start Øphio again.";
const LOCKED: &str = "This Mac is locked";

/// What the phone is told for what can and can't be done.
/// `held` says why the screen can't be used now, given the permissions: the
/// Mac is locked, or its screen is asleep.
fn capabilities(capture: bool, input: bool, held: Option<&str>) -> DesktopCapabilities {
    let or_missing = |given: bool, remedy: &str| {
        if !given {
            Availability::PermissionMissing {
                remedy: remedy.into(),
            }
        } else if let Some(reason) = held {
            Availability::TemporarilyUnavailable {
                reason: reason.into(),
            }
        } else {
            Availability::Available
        }
    };
    DesktopCapabilities {
        backend: "macos".into(),
        capture: or_missing(capture, SCREEN_RECORDING),
        input: or_missing(input, ACCESSIBILITY),
        input_scope: if input {
            InputScope::FocusedTarget
        } else {
            InputScope::None
        },
        lock_session: Availability::Available,
        unlock_session: Availability::Unsupported {
            reason: "A Mac lets only its own login screen unlock it".into(),
        },
    }
}

impl Driver for MacDriver {
    fn monitors(&self) -> Result<Vec<Monitor>, PlatformError> {
        native::monitors()
    }
    fn capabilities(&self) -> DesktopCapabilities {
        let (capture, input) = native::permissions();
        let held = if native::lock_state() == DesktopLockState::Locked {
            Some(LOCKED)
        } else if native::asleep() {
            Some(native::ASLEEP)
        } else {
            None
        };
        capabilities(capture, input, held)
    }
    fn capture(&self) -> Result<Box<dyn Capture>, PlatformError> {
        if !native::permissions().0 {
            return Err(PlatformError::PermissionMissing(SCREEN_RECORDING.into()));
        }
        Ok(Box::new(capture::MacCapture::new()))
    }
    fn injector(&self) -> Result<Box<dyn Injector>, PlatformError> {
        if !native::permissions().1 {
            return Err(PlatformError::PermissionMissing(ACCESSIBILITY.into()));
        }
        Ok(Box::new(input::MacInjector::new()?))
    }
    fn lock(&self) -> Result<(), PlatformError> {
        native::lock()
    }
    fn lock_state(&self) -> DesktopLockState {
        native::lock_state()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_permission_says_where_to_give_it() {
        let none = capabilities(false, false, None);
        for (availability, place) in [
            (&none.capture, "Screen & System Audio Recording"),
            (&none.input, "Accessibility"),
        ] {
            let Availability::PermissionMissing { remedy } = availability else {
                panic!("{availability:?}");
            };
            assert!(remedy.contains(place), "{remedy}");
        }
        assert_eq!(none.input_scope, InputScope::None);
        assert_eq!(none.lock_session, Availability::Available);
    }

    #[test]
    fn a_locked_mac_is_neither_watched_nor_controlled() {
        let locked = capabilities(true, true, Some(LOCKED));
        assert!(matches!(
            locked.capture,
            Availability::TemporarilyUnavailable { .. }
        ));
        assert!(matches!(
            locked.input,
            Availability::TemporarilyUnavailable { .. }
        ));
    }

    #[test]
    fn a_sleeping_screen_says_so_and_how_to_see_it_again() {
        let asleep = capabilities(true, true, Some(native::ASLEEP));
        let Availability::TemporarilyUnavailable { reason } = &asleep.capture else {
            panic!("{:?}", asleep.capture);
        };
        assert_eq!(reason, "The Mac’s screen is asleep. Wake it to see it.");
        // A missing permission is still what to fix first.
        assert!(matches!(
            capabilities(false, true, Some(native::ASLEEP)).capture,
            Availability::PermissionMissing { .. }
        ));
    }

    #[test]
    fn with_permissions_it_is_available_and_can_never_be_unlocked_from_here() {
        let given = capabilities(true, true, None);
        assert_eq!(given.capture, Availability::Available);
        assert_eq!(given.input, Availability::Available);
        assert_eq!(given.input_scope, InputScope::FocusedTarget);
        assert!(matches!(
            given.unlock_session,
            Availability::Unsupported { .. }
        ));
    }

    #[test]
    fn screens_are_listed_with_their_sizes_where_there_are_any() {
        // A session with no screen has none to list, and says so.
        match native::monitors() {
            Ok(monitors) => {
                assert!(!monitors.is_empty());
                for monitor in monitors {
                    assert!(monitor.width > 0 && monitor.height > 0, "{monitor:?}");
                    assert_eq!(monitor.dpi, 96);
                    assert!(!monitor.name.is_empty());
                }
            }
            Err(error) => {
                assert!(
                    matches!(
                        error,
                        PlatformError::TemporarilyUnavailable(_) | PlatformError::ScreenAsleep(_)
                    ),
                    "{error:?}"
                );
            }
        }
    }

    #[test]
    fn asking_about_the_lock_and_permissions_is_safe_anywhere() {
        let _ = native::lock_state();
        let _ = native::permissions();
    }
}
