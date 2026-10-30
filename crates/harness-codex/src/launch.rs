// SPDX-License-Identifier: Apache-2.0
//! CLI-equivalent policies for app-server, which does not accept TUI flags.
use ports::{HarnessError, RunSpec};
use protocol::{
    capability::HarnessKind,
    model::{LaunchPolicy, TaskMode},
};
use serde_json::{Value, json};

pub fn validate(spec: &RunSpec) -> Result<(), HarnessError> {
    if spec
        .launch_policy
        .is_some_and(|p| !p.supports(HarnessKind::Codex))
    {
        return Err(HarnessError::Unsupported(
            "These launch flags are not supported by Codex.".into(),
        ));
    }
    Ok(())
}

pub fn policy(spec: &RunSpec) -> Option<(&'static str, &'static str, &'static str)> {
    if spec.mode != TaskMode::Work {
        return None;
    }
    match spec.launch_policy? {
        LaunchPolicy::Standard => Some(("on-request", "workspace-write", "workspaceWrite")),
        LaunchPolicy::BypassPermissions => {
            Some(("never", "danger-full-access", "dangerFullAccess"))
        }
        LaunchPolicy::Sandboxed => Some(("never", "workspace-write", "workspaceWrite")),
        LaunchPolicy::AcceptEdits => None, // Refused before spawning.
    }
}

pub fn arguments(spec: &RunSpec, args: &mut Vec<String>) {
    if let Some((approval, sandbox, _)) = policy(spec) {
        for setting in [
            format!("approval_policy=\"{approval}\""),
            format!("sandbox_mode=\"{sandbox}\""),
        ] {
            args.extend(["-c".into(), setting]);
        }
    }
}

pub fn thread(spec: &RunSpec, params: &mut Value) {
    if let Some((approval, sandbox, _)) = policy(spec) {
        params["approvalPolicy"] = json!(approval);
        params["approvalsReviewer"] = json!("user");
        params["sandbox"] = json!(sandbox);
    }
}

pub fn turn(spec: &RunSpec, params: &mut Value) {
    if let Some((approval, _, sandbox)) = policy(spec) {
        params["approvalPolicy"] = json!(approval);
        params["approvalsReviewer"] = json!("user");
        params["sandboxPolicy"] = if sandbox == "workspaceWrite" {
            json!({"type":"workspaceWrite", "writableRoots":[spec.cwd], "networkAccess":false,
                "excludeTmpdirEnvVar":false, "excludeSlashTmp":false})
        } else {
            json!({"type":sandbox})
        };
    }
}

/// Never send the prompt if native policy was ignored or changed by configuration.
pub fn confirms(spec: &RunSpec, response: &Value) -> bool {
    policy(spec).is_some_and(|(approval, _, sandbox)| {
        response["approvalPolicy"] == approval
            && response["approvalsReviewer"] == "user"
            && response["sandbox"]["type"] == sandbox
    })
}

pub const RELAY_PROVIDER: &str = "ophio_relay";
pub const RELAY_KEY_ENV: &str = "OPHIO_RELAY_DUMMY_KEY";
pub fn relay(spec: &RunSpec) -> Option<ports::sandbox::RelayEndpoint> {
    spec.sandbox.as_ref().and_then(|s| s.relay())
}
pub fn provider(spec: &RunSpec) -> &'static str {
    if relay(spec).is_some() {
        RELAY_PROVIDER
    } else {
        "openai"
    }
}
pub fn relay_arguments(spec: &RunSpec, args: &mut Vec<String>) -> Result<(), HarnessError> {
    if let Some(endpoint) = relay(spec) {
        if endpoint.provider != ports::sandbox::RelayProvider::OpenAi
            || spec.bridge.is_some()
            || spec.resume_session.is_some()
        {
            return Err(HarnessError::Unsupported(
                "Relay runs need the matching provider, fresh state and no host tool bridge."
                    .into(),
            ));
        }
        for setting in [
            "forced_login_method=\"api\"".to_owned(),
            format!("model_provider=\"{RELAY_PROVIDER}\""),
            format!("model_providers.{RELAY_PROVIDER}.name=\"Ophio relay\""),
            format!(
                "model_providers.{RELAY_PROVIDER}.base_url=\"{}/v1\"",
                endpoint.base_url()
            ),
            format!("model_providers.{RELAY_PROVIDER}.wire_api=\"responses\""),
            format!("model_providers.{RELAY_PROVIDER}.env_key=\"{RELAY_KEY_ENV}\""),
            format!("model_providers.{RELAY_PROVIDER}.requires_openai_auth=false"),
            format!("model_providers.{RELAY_PROVIDER}.supports_websockets=false"),
            "analytics.enabled=false".to_owned(),
        ] {
            args.extend(["-c".into(), setting]);
        }
    }
    Ok(())
}
