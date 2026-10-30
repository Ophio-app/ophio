// SPDX-License-Identifier: Apache-2.0
use ports::desktop::DesktopCapabilities;
use protocol::capability::{Availability, InputScope};
use std::collections::BTreeSet;

/// Supplied by the composition root; detection never reads process environment or opens a display.
#[derive(Debug, Clone, Default)]
pub struct Environment {
    pub session_type: String,
    pub current_desktop: String,
    pub wayland_globals: BTreeSet<String>,
    pub portal_available: bool,
    pub x11_available: bool,
    pub session_lock_available: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    Wlroots,
    Portal,
    X11,
    Unsupported,
}
#[derive(Debug, Clone)]
pub struct Detection {
    pub kind: BackendKind,
    pub capabilities: DesktopCapabilities,
}

pub(crate) fn detect(env: &Environment) -> Detection {
    let has = |name: &str| env.wayland_globals.contains(name);
    let native = (has("zwlr_screencopy_manager_v1")
        || (has("ext_image_copy_capture_manager_v1")
            && has("ext_output_image_capture_source_manager_v1")))
        && has("wl_shm")
        && has("zwlr_virtual_pointer_manager_v1")
        && has("zwp_virtual_keyboard_manager_v1")
        && has("wl_seat");
    let kind = match env.session_type.to_ascii_lowercase().as_str() {
        "wayland" if native => BackendKind::Wlroots,
        "wayland" if env.portal_available => BackendKind::Portal,
        "x11" if env.x11_available => BackendKind::X11,
        _ => BackendKind::Unsupported,
    };
    let ready = match kind {
        BackendKind::Wlroots | BackendKind::X11 => Availability::Available,
        BackendKind::Portal => Availability::PermissionMissing {
            remedy: "Approve screen sharing and remote input in the desktop permission dialog."
                .into(),
        },
        BackendKind::Unsupported => Availability::Unsupported {
            reason: "No supported capture and input route is advertised by this session.".into(),
        },
    };
    Detection {
        kind,
        capabilities: DesktopCapabilities {
            backend: match kind {
                BackendKind::Wlroots => "wlroots",
                BackendKind::Portal => "portal",
                BackendKind::X11 => "x11",
                BackendKind::Unsupported => "none",
            }
            .into(),
            capture: ready.clone(),
            input: ready,
            input_scope: if kind == BackendKind::Unsupported {
                InputScope::None
            } else {
                InputScope::FocusedTarget
            },
            lock_session: if env.session_lock_available {
                Availability::Available
            } else {
                Availability::Unsupported {
                    reason: "No session lock service configured.".into(),
                }
            },
            unlock_session: Availability::Unsupported {
                reason: "No session unlocker configured.".into(),
            },
        },
    }
}
