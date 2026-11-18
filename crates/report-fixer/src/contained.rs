// SPDX-License-Identifier: Apache-2.0
//! The sandbox report fixes and their checks run in, held to a disk budget.
//! Only Linux has one. Anywhere else this computer is never ready for fixes,
//! and every run is refused before anything starts.
use crate::{Error, Result, execution::Limits};
use ports::sandbox::{Sandbox, SandboxSpec};
use std::{path::Path, sync::Arc};

#[cfg(target_os = "linux")]
pub(crate) use sandbox::DiskBudget;

/// Off Linux there is no disk budget, so no contained run can exist.
#[cfg(not(target_os = "linux"))]
#[derive(Debug)]
pub(crate) enum DiskBudget {}
#[cfg(not(target_os = "linux"))]
impl DiskBudget {
    pub(crate) fn exceeded(&self) -> std::io::Result<bool> {
        match *self {}
    }
}

/// Whether this computer can hold a run in.
pub(crate) fn available(shim: &Path) -> bool {
    #[cfg(target_os = "linux")]
    {
        use ports::sandbox::Sandboxes;
        sandbox::Bubblewrap::new(shim).availability().is_available()
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = shim;
        false
    }
}

/// A sandbox for `spec`, held to the disk budget in `limits`. `refusal` is
/// the error when the sandbox can't be made.
pub(crate) fn create(
    shim: &Path,
    limits: &Limits,
    spec: SandboxSpec,
    refusal: &'static str,
) -> Result<(Arc<dyn Sandbox>, Arc<DiskBudget>)> {
    #[cfg(target_os = "linux")]
    {
        use ports::sandbox::Sandboxes;
        let disk = Arc::new(DiskBudget::new(limits.disk_bytes, limits.disk_entries)?);
        let sandbox = sandbox::Bubblewrap::new(shim)
            .with_disk_budget(disk.clone())
            .create(spec)
            .map_err(|_| Error::Operation(refusal))?;
        Ok((sandbox, disk))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (shim, limits, spec);
        Err(Error::Operation(refusal))
    }
}

#[cfg(all(test, not(target_os = "linux")))]
mod tests {
    #[test]
    fn nothing_is_contained_off_linux() {
        assert!(!super::available(std::path::Path::new(
            "ophio-sandbox-shim"
        )));
    }
}
