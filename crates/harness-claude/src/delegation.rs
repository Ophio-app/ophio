// SPDX-License-Identifier: Apache-2.0
//! Native subagent isolation: the parent's tool pool contains only delegation.
//! https://code.claude.com/docs/en/sub-agents#scope-mcp-servers-to-a-subagent
use ports::{HarnessError, RunSpec};
use protocol::capability::Availability;
use serde_json::json;

pub(crate) const TOOLS: [&str; 6] = ports::bridge::COMPUTER_TOOLS;
pub(crate) const SPECIALIST: &str = "computer-specialist";
const SUPERVISOR: &str = "computer-supervisor";

pub(crate) async fn availability(binary: &std::path::Path) -> Availability {
    availability_in(super::io::inspect(binary, &["--help"]).await.as_ref())
}
/// Whether `claude --help`, as `help` printed it, offers what delegation needs.
pub(crate) fn availability_in(help: Option<&std::process::Output>) -> Availability {
    if help.is_some_and(|output| {
        output.status.success()
            && [
                "--agents",
                "--agent",
                "--forward-subagent-text",
                "--restricted",
                "--setting-sources",
                "--settings",
                "--strict-mcp-config",
                "--tools",
                "--allowedTools",
                "--disable-slash-commands",
                "--no-chrome",
                "--system-prompt-snapshot",
            ]
            .iter()
            .all(|flag| String::from_utf8_lossy(&output.stdout).contains(flag))
    }) {
        Availability::Available
    } else {
        Availability::Unsupported { reason: "This Claude Code version does not expose the named-agent, isolated tool configuration and child-message forwarding needed for computer use".into() }
    }
}

pub(crate) fn arguments(spec: &RunSpec) -> Result<Vec<String>, HarnessError> {
    let bridge = spec
        .bridge
        .as_ref()
        .filter(|b| b.computer_use)
        .ok_or_else(|| HarnessError::Protocol("computer-use bridge missing".into()))?;
    super::io::bridge_name(&bridge.server_name)?;
    super::io::validate_env(&bridge.env)?;
    let tools: Vec<_> = TOOLS
        .iter()
        .map(|name| format!("mcp__{}__{name}", bridge.server_name))
        .collect();
    let agents = json!({
        SUPERVISOR: {
            "description": "Coordinates the reviewed computer task",
            "prompt": "Delegate the person's goal to computer-specialist in the foreground. You have no desktop tools. Report the specialist's result and any unconfirmed outcome. Never invent observations.",
            "tools": [format!("Agent({SPECIALIST})")],
            "model": "inherit"
        },
        SPECIALIST: {
            "description": "Performs the reviewed goal on the bound desktop target",
            "prompt": ports::harness::COMPUTER_INSTRUCTION,
            "tools": tools,
            "model": spec.model.as_deref().unwrap_or("inherit"),
            "mcpServers": [{ &bridge.server_name: {"type":"stdio", "command":bridge.command, "args":bridge.args} }]
        }
    });
    let mut allowed = tools;
    allowed.push(format!("Agent({SPECIALIST})"));
    Ok(vec![
        "--agents".into(),
        agents.to_string(),
        "--agent".into(),
        SUPERVISOR.into(),
        "--tools".into(),
        "Agent".into(),
        "--allowedTools".into(),
        allowed.join(","),
        "--restricted".into(),
        "--setting-sources".into(),
        String::new(),
        "--settings".into(),
        json!({"disableAllHooks":true}).to_string(),
        "--strict-mcp-config".into(),
        "--mcp-config".into(),
        "{\"mcpServers\":{}}".into(),
        "--disable-slash-commands".into(),
        "--no-chrome".into(),
        "--forward-subagent-text".into(),
        "--system-prompt-snapshot".into(),
        "off".into(),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bridge_is_child_scoped_and_neither_agent_has_arbitrary_tools() {
        let (_, _, mut spec) = crate::tests::fixture();
        spec.bridge.as_mut().unwrap().computer_use = true;
        let args = arguments(&spec).unwrap();
        let definitions: serde_json::Value = serde_json::from_str(&args[1]).unwrap();
        let parent = &definitions[SUPERVISOR];
        let child = &definitions[SPECIALIST];
        assert_eq!(parent["tools"], json!(["Agent(computer-specialist)"]));
        assert!(parent.get("mcpServers").is_none());
        assert_eq!(child["tools"].as_array().unwrap().len(), 6);
        assert_eq!(
            child["mcpServers"][0]["computer"]["command"],
            "/fixture/computer-mcp"
        );
        assert!(
            args.windows(2)
                .any(|w| w == ["--mcp-config", "{\"mcpServers\":{}}"])
        );
        assert!(!args.join(" ").contains("fixture-secret"));
    }
}
