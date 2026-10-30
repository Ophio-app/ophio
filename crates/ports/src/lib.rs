// SPDX-License-Identifier: Apache-2.0
//! Interfaces between the companion's services and the outside world: native
//! agent harnesses, desktop capture/input backends, media encoding, library
//! files, terminals, speech recognition, the computer-use bridge and time.
//! Services depend only on these traits; concrete adapters are wired
//! together in the companion binary.

pub mod accounts;
pub mod agent_browser;
pub mod agent_copy;
pub mod agent_logins;
pub mod bridge;
pub mod checks;
pub mod clock;
pub mod desktop;
pub mod github;
pub mod harness;
pub mod library;
pub mod media;
pub mod model_name;
pub mod sandbox;
pub mod servers;
pub mod speech;
pub mod system;
pub mod terminal;
pub mod tool_approval;

pub use clock::{Clock, SystemClock};
pub use desktop::{
    CapturedFrame, DesktopBackend, DesktopCapabilities, FrameStream, InputSink, PixelFormat,
    PlatformError, UnlockOutcome,
};
pub use harness::{
    Containment, Disabled, DisabledEntry, HarnessAdapter, HarnessError, HarnessEvent, ImageInput,
    MessageRef, NativeApproval, NativeEntry, PromptInput, RunControl, RunEnv, RunExit, RunSpec,
    RunStart, RunStarted, SCRATCH_ENV, TASK_ENV, TerminalBlock, ToolBridge,
};
pub use library::{LibraryChange, LibraryError, LibraryFs, RootSpec, SaveRequest, TrashEntry};
pub use media::{
    EncodedOut, EncodedStill, EncodedStream, EncodedStreamControl, MediaEncoder, StillFormat,
};
pub use speech::{SpeechEngine, SpeechError, SpeechSession};
pub use terminal::{Replay, TerminalError, TerminalEvent, TerminalHost, TerminalSpec};

pub mod windows;

/// The product's name as people see it.
pub const PRODUCT_NAME: &str = "Øphio";
/// The program on the computer: the command people type, its login service and
/// its settings and data directories.
pub const PROGRAM_NAME: &str = "ophio";
/// The program as typed in a terminal on this computer. The Windows archive is
/// not put on `PATH`, so there it runs from its own folder.
pub const COMMAND: &str = if cfg!(windows) {
    ".\\ophio.exe"
} else {
    PROGRAM_NAME
};
/// The start of the name of each cgroup the companion with process id
/// `companion` makes for an agent run, inside its own.
pub fn run_cgroup_prefix(companion: u32) -> String {
    format!("{PROGRAM_NAME}-run-{companion}-")
}
/// The companion that made the cgroup named `name`, when a companion made it:
/// the name is [`run_cgroup_prefix`] and a number, exactly.
pub fn run_cgroup_companion(name: &str) -> Option<u32> {
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let (companion, number) = name
        .strip_prefix(PROGRAM_NAME)?
        .strip_prefix("-run-")?
        .split_once('-')?;
    (digits(companion) && digits(number))
        .then(|| companion.parse().ok())
        .flatten()
}
/// The program's name before the product was named. An earlier install keeps
/// its settings, data and login startup under it until they move.
pub const EARLIER_PROGRAM_NAME: &str = "companion";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_run_cgroup_names_the_companion_that_made_it() {
        let name = format!("{}7", run_cgroup_prefix(1234));
        assert_eq!(run_cgroup_companion(&name), Some(1234));
        for other in [
            "ophio-run-1234-",
            "ophio-run--7",
            "ophio-run-12a-7",
            "ophio-run-12-7b",
            "ophio-run-12-7-1",
            "xophio-run-12-7",
            "ophio-runs-12-7",
            "ophio-run-99999999999-7",
        ] {
            assert_eq!(run_cgroup_companion(other), None, "{other}");
        }
    }
}
