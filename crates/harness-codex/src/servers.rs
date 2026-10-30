// SPDX-License-Identifier: Apache-2.0
//! The person's own MCP servers. Setting Codex's server table empty leaves
//! the configured ones in place, so a run that mustn't reach them, a
//! read-only one or computer use, switches each off by name.

use crate::io;
use ports::{HarnessError, RunSpec};
use serde_json::Value;
use std::{path::Path, process::Stdio, time::Duration};

/// The names of the MCP servers Codex would start for this run, from its
/// settings and the project's.
pub(crate) async fn listed(binary: &Path, spec: &RunSpec) -> Result<Vec<String>, HarnessError> {
    let unread = || {
        HarnessError::Unsupported(
            "Codex couldn’t list its MCP servers, so the task didn’t start: Ophio switches them off for read-only tasks and computer use. Check that `codex mcp list` works on this computer.".into(),
        )
    };
    let mut command = io::command(binary);
    command
        .args(["mcp", "list", "--json"])
        .current_dir(&spec.cwd)
        .envs(spec.env.0.iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::null());
    let output = tokio::time::timeout(Duration::from_secs(10), command.output())
        .await
        .map_err(|_| unread())?
        .map_err(|_| unread())?;
    if !output.status.success() {
        return Err(unread());
    }
    let listed: Value = serde_json::from_slice(&output.stdout).map_err(|_| unread())?;
    let Value::Array(servers) = listed else {
        return Err(unread());
    };
    servers
        .iter()
        .map(|server| {
            server["name"]
                .as_str()
                .map(str::to_owned)
                .ok_or_else(unread)
        })
        .collect()
}

/// The setting that switches `names` off. It goes before any server Ophio
/// adds: a later setting of a server's table merges into it.
pub(crate) fn off(names: &[String]) -> Option<String> {
    if names.is_empty() {
        return None;
    }
    let off: Vec<String> = (names.iter())
        .map(|name| format!("{}={{enabled=false}}", crate::toml_string(name)))
        .collect();
    Some(format!("mcp_servers={{{}}}", off.join(",")))
}

#[cfg(test)]
mod tests {
    #[test]
    fn each_server_is_switched_off_by_its_quoted_name() {
        assert_eq!(super::off(&[]), None);
        assert_eq!(
            super::off(&["mine".into(), "we \"ird\"".into()]).as_deref(),
            Some(r#"mcp_servers={"mine"={enabled=false},"we \"ird\""={enabled=false}}"#)
        );
    }
}
