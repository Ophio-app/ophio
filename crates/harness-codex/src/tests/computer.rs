// SPDX-License-Identifier: Apache-2.0
use super::*;

/// A computer-use run on a Codex computer use was checked with, with the
/// models it fetched and two servers of the person's own.
fn computer_fixture() -> (tempfile::TempDir, CodexAdapter, RunSpec) {
    let (dir, adapter, mut spec) = fixture();
    std::fs::write(dir.path().join("version"), "codex-cli 0.161.0").unwrap();
    let home = dir.path().join("codex-home");
    std::fs::create_dir(&home).unwrap();
    let model = |slug: &str| {
        json!({"slug":slug,"apply_patch_tool_type":"freeform","multi_agent_version":"v2",
            "experimental_supported_tools":["fixture_tool"],"supports_search_tool":true,"priority":1})
    };
    std::fs::write(
        home.join("models_cache.json"),
        json!({"fetched_at":"2026-10-09T00:00:00Z","models":[model("requested-model"),model("actual-native-model")]}).to_string(),
    )
    .unwrap();
    std::fs::write(
        dir.path().join("mcp-servers.json"),
        r#"[{"name":"mine","enabled":true},{"name":"we ird","enabled":true}]"#,
    )
    .unwrap();
    spec.env
        .0
        .push(("CODEX_HOME".into(), home.to_string_lossy().into_owned()));
    spec.bridge.as_mut().unwrap().computer_use = true;
    spec.resume_session = None;
    spec.prompt.text = "computer".into();
    (dir, adapter, spec)
}

fn frames(dir: &tempfile::TempDir) -> Vec<Value> {
    std::fs::read_to_string(dir.path().join("stdin.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

async fn exit(run: &mut RunStarted, warnings: &mut Vec<String>) -> RunExit {
    loop {
        match event(&mut run.events).await {
            HarnessEvent::Exited { exit, .. } => return exit,
            HarnessEvent::Warning { text } => warnings.push(text),
            _ => {}
        }
    }
}

#[tokio::test]
async fn computer_use_is_offered_from_the_codex_version_it_was_checked_with() {
    let (dir, adapter, _spec) = fixture();
    assert!(
        matches!(adapter.probe().await.capabilities.computer_use_delegation, Availability::Unsupported { reason } if reason.contains("0.161"))
    );
    std::fs::write(dir.path().join("version"), "codex-cli 0.161.0").unwrap();
    assert!(matches!(
        adapter.probe().await.capabilities.computer_use_delegation,
        Availability::Available
    ));
}

#[tokio::test]
async fn computer_use_runs_read_only_with_the_six_tools_and_nothing_of_its_own() {
    let (dir, adapter, spec) = computer_fixture();
    let mut run = adapter.start_run(spec).await.unwrap();
    let mut warnings = vec![];
    assert_eq!(exit(&mut run, &mut warnings).await, RunExit::Completed);

    let args: Vec<String> =
        serde_json::from_str(&std::fs::read_to_string(dir.path().join("argv.json")).unwrap())
            .unwrap();
    let raw = args.join("\n");
    assert!(!raw.contains("fixture-secret"));
    for feature in [
        "shell_tool",
        "unified_exec",
        "computer_use",
        "browser_use",
        "apps",
        "plugins",
        "hooks",
        "multi_agent",
        "multi_agent_v2",
        "view_image",
        "image_generation",
    ] {
        assert!(
            args.contains(&format!("features.{feature}=false")),
            "{feature}"
        );
    }
    assert!(
        !args
            .iter()
            .any(|a| a.starts_with("features.") && a.ends_with("=true"))
    );
    for setting in [
        "web_search=\"disabled\"",
        "apps={_default={enabled=false}}",
        "sandbox_mode=\"read-only\"",
        "approval_policy=\"never\"",
    ] {
        assert!(args.iter().any(|a| a == setting), "{setting}");
    }
    // The person's servers are off before the bridge's is added.
    let off = (args.iter())
        .position(|a| a == r#"mcp_servers={"mine"={enabled=false},"we ird"={enabled=false}}"#)
        .unwrap();
    let bridge = args
        .iter()
        .position(|a| a.starts_with("mcp_servers.computer="))
        .unwrap();
    assert!(off < bridge);
    let tools: Vec<String> = (ports::bridge::COMPUTER_TOOLS.iter())
        .map(|t| format!("\"{t}\""))
        .collect();
    assert!(args[bridge].contains(&format!("enabled_tools=[{}]", tools.join(","))));
    // Its catalog has the models Codex fetched, less the tools each brings.
    let catalog = (args.iter())
        .find_map(|a| a.strip_prefix("model_catalog_json="))
        .unwrap();
    assert!(catalog.contains("ophio-codex-models-"));

    let frames = frames(&dir);
    let thread = frames
        .iter()
        .find(|f| f["method"] == "thread/start")
        .unwrap();
    assert_eq!(thread["params"]["sandbox"], "read-only");
    assert_eq!(thread["params"]["approvalPolicy"], "never");
    let told = thread["params"]["developerInstructions"].as_str().unwrap();
    assert!(
        told.contains(ports::harness::COMPUTER_INSTRUCTION)
            && told.contains(crate::computer::ASKING)
    );
    assert!(!told.contains(ASKING));
    // It reports what it came to, with none of the bridge's waiting or asking.
    assert!(told.contains("ophio-outcome"));
    assert!(!told.contains("wake_me") && !told.contains("can_ask"));
    let turn = frames.iter().find(|f| f["method"] == "turn/start").unwrap();
    assert_eq!(
        turn["params"]["sandboxPolicy"],
        json!({"type":"readOnly","networkAccess":false})
    );
    assert_eq!(turn["params"]["approvalPolicy"], "never");
    // What it asks for besides the tools is declined, and said so.
    let answer = |id: i64| {
        frames
            .iter()
            .find(|f| f["id"] == id && f.get("method").is_none())
            .unwrap()["result"]
            .clone()
    };
    assert_eq!(answer(71), json!({"decision":"decline"}));
    assert_eq!(answer(72), json!({"answers":{}}));
    assert_eq!(
        warnings,
        ["Codex asked for something outside computer use, so it was declined"; 2]
    );
}

#[tokio::test]
async fn computer_use_stops_a_run_that_reaches_for_anything_else() {
    let (_dir, adapter, mut spec) = computer_fixture();
    // The fixture's usual turn runs a command and changes a file.
    spec.prompt.text = "start".into();
    let mut run = adapter.start_run(spec).await.unwrap();
    let exit = exit(&mut run, &mut vec![]).await;
    assert_eq!(
        exit,
        RunExit::Failed {
            reason: "Codex tried to run a command, which computer use doesn’t allow, so Ophio stopped the task.".into()
        }
    );
}

#[tokio::test]
async fn computer_use_does_not_start_where_it_cannot_be_kept_to_its_tools() {
    async fn refused(adapter: &CodexAdapter, spec: RunSpec, words: &str) {
        match adapter.start_run(spec).await {
            Err(HarnessError::Unsupported(reason)) => assert!(reason.contains(words), "{reason}"),
            Err(other) => panic!("{other:?}"),
            Ok(_) => panic!("started"),
        }
    }
    let (dir, adapter, spec) = computer_fixture();
    let mut ask = spec.clone();
    ask.mode = TaskMode::Ask;
    refused(&adapter, ask, "Work").await;
    let mut flagged = spec.clone();
    flagged.launch_policy = Some(protocol::model::LaunchPolicy::Standard);
    refused(&adapter, flagged, "launch flags").await;
    let mut unlisted = spec.clone();
    unlisted.model = Some("elsewhere-model".into());
    refused(&adapter, unlisted, "doesn’t list elsewhere-model").await;
    std::fs::write(
        dir.path().join("mcp-servers.json"),
        r#"[{"name":"computer"}]"#,
    )
    .unwrap();
    refused(&adapter, spec.clone(), "Rename it").await;
    std::fs::write(dir.path().join("mcp-list-fails"), "").unwrap();
    refused(&adapter, spec.clone(), "codex mcp list").await;
    std::fs::remove_file(dir.path().join("mcp-list-fails")).unwrap();
    std::fs::remove_file(dir.path().join("codex-home/models_cache.json")).unwrap();
    refused(&adapter, spec, "Open Codex once").await;
    assert!(!dir.path().join("stdin.jsonl").exists());
}

#[tokio::test]
async fn computer_use_ends_before_a_turn_when_codex_would_not_stay_read_only_or_on_its_models() {
    let (dir, adapter, spec) = computer_fixture();
    std::fs::write(dir.path().join("unsafe-response"), "").unwrap();
    let mut run = adapter.start_run(spec.clone()).await.unwrap();
    assert!(
        matches!(exit(&mut run, &mut vec![]).await, RunExit::Failed { reason } if reason.contains("stay read-only"))
    );
    std::fs::remove_file(dir.path().join("unsafe-response")).unwrap();
    // Codex answers with a model the run's catalog doesn't list.
    let home = dir.path().join("codex-home/models_cache.json");
    std::fs::write(
        &home,
        json!({"models":[{"slug":"requested-model"}]}).to_string(),
    )
    .unwrap();
    std::fs::remove_file(dir.path().join("stdin.jsonl")).unwrap();
    let mut run = adapter.start_run(spec).await.unwrap();
    assert!(
        matches!(exit(&mut run, &mut vec![]).await, RunExit::Failed { reason } if reason.contains("doesn’t list on this computer"))
    );
    assert!(!frames(&dir).iter().any(|f| f["method"] == "turn/start"));
}

#[tokio::test]
async fn the_run_catalog_drops_what_each_model_brings_by_itself() {
    let (_dir, _adapter, spec) = computer_fixture();
    let launch = crate::computer::Launch::prepare(&spec, "computer", &[])
        .await
        .unwrap();
    let args = launch.arguments();
    let path = (args.iter())
        .find_map(|a| a.strip_prefix("model_catalog_json="))
        .unwrap()
        .trim_matches('"')
        .to_owned();
    let catalog: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    for model in catalog["models"].as_array().unwrap() {
        assert!(model["apply_patch_tool_type"].is_null() && model["multi_agent_version"].is_null());
        assert_eq!(model["experimental_supported_tools"], json!([]));
        assert_eq!(model["supports_search_tool"], false);
        assert_eq!(model["priority"], 1);
    }
    assert_eq!(launch.models.len(), 2);
}
