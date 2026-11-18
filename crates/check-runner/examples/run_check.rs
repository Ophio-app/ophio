// SPDX-License-Identifier: Apache-2.0
//! Run a disposable offline sample, not an approved lifecycle job.
use check_runner::runner::{self, CheckOutcome, CheckRequest};
use std::{io, time::Duration};

#[tokio::main]
async fn main() -> io::Result<()> {
    let mut args = std::env::args_os().skip(1);
    let project = args.next().ok_or_else(|| {
        io::Error::other("Usage: run_check PROJECT COMMAND [READ_ONLY_TOOLCHAIN_FOLDER ...]")
    })?;
    let command = args
        .next()
        .ok_or_else(|| io::Error::other("A shell command is required"))?
        .into_string()
        .map_err(|_| io::Error::other("The command must be valid text"))?;
    let data = tempfile::tempdir()?;
    let mut request = CheckRequest::new(
        project,
        data.path().canonicalize()?,
        command,
        Duration::from_secs(120),
    );
    request.read_toolchains = args.map(std::path::PathBuf::from).collect();
    let result = runner::start(request)
        .await
        .map_err(io::Error::other)?
        .wait()
        .await?;
    println!(
        "{}",
        serde_json::to_string_pretty(&result).map_err(io::Error::other)?
    );
    if result.outcome != CheckOutcome::Passed {
        return Err(io::Error::other("The sample check did not pass"));
    }
    Ok(())
}
