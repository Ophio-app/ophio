// SPDX-License-Identifier: Apache-2.0
//! Live opt-in test; performs inference using the installed native subscription.
#[cfg(target_os = "linux")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    live::main()
}
#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("This example requires Linux bubblewrap.");
}
#[cfg(target_os = "linux")]
mod live {
    use ports::{
        HarnessAdapter, HarnessEvent, RunSpec,
        sandbox::{Reach, SandboxSpec, Sandboxes},
    };
    use protocol::{
        capability::HarnessKind,
        ids::{RunId, TaskId},
        model::{LaunchPolicy, TaskMode},
    };
    use std::{
        path::PathBuf,
        sync::Arc,
        time::{Duration, SystemTime, UNIX_EPOCH},
    };
    fn now() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }
    #[tokio::main]
    pub async fn main() -> Result<(), Box<dyn std::error::Error>> {
        let claude = std::env::args().any(|a| a == "--claude");
        let root = tempfile::tempdir()?;
        let paths: Vec<_> = ["A", "B", "C", "outside", "scratch", "private"]
            .iter()
            .map(|p| root.path().join(p))
            .collect();
        for p in &paths {
            std::fs::create_dir(p)?;
        }
        std::fs::write(paths[1].join("note.txt"), "B_SENTINEL\n")?;
        std::fs::write(paths[3].join("secret.txt"), "OUTSIDE_SENTINEL\n")?;
        std::fs::write(paths[5].join("secret.txt"), "PRIVATE_SENTINEL\n")?;
        let shim = std::env::current_exe()?
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("ophio-sandbox-shim");
        let backend = sandbox::Bubblewrap::new(shim);
        let kind = if claude {
            HarnessKind::ClaudeCode
        } else {
            HarnessKind::Codex
        };
        let mut reach = Reach {
            read: vec![paths[0].clone(), paths[2].clone()],
            write: vec![],
            whole_read: false,
            whole_write: false,
            internet: false,
            local_network: false,
        };
        let sandbox = backend.create(SandboxSpec {
            offline: false,
            reach: reach.clone(),
            harness: kind,
            cwd: paths[0].clone(),
            state: root.path().join("state"),
            always: vec![paths[4].clone()],
            programs: vec![],
            candidates: paths[..3].to_vec(),
            private: vec![paths[5].clone()],
            given: vec![],
            relay: None,
            on_blocked: Some(Arc::new(|b| println!("BLOCK {} {b:?}", now()))),
        })?;
        let script = format!(
            r#"#!/bin/bash
for i in $(seq 0 17); do
  echo "ATTEMPT=$i TIME=$(date -u +%FT%TZ) SHELL_PID=$$"
  echo LIST; ls '{b}' 2>&1; echo "LIST_RC=$?"
  echo READ; cat '{b}/note.txt' 2>&1; echo "READ_RC=$?"
  echo WRITE; (echo "attempt-$i" > '{c}/out-'$i.txt) 2>&1; echo "WRITE_RC=$?"
  echo NET; curl -sS -I --max-time 5 https://example.com 2>&1; echo "NET_RC=$?"
  echo OUTSIDE; cat '{outside}/secret.txt' 2>&1; echo "OUTSIDE_RC=$?"
  echo PRIVATE; cat '{private}/secret.txt' 2>&1; echo "PRIVATE_RC=$?"
  echo DIRECT_NET; curl --noproxy '*' -sS -I --connect-timeout 1 --max-time 1 https://1.1.1.1 2>&1; echo "DIRECT_RC=$?"
  echo "$i" > '{scratch}/tick'
  sleep 10
done
"#,
            b = paths[1].display(),
            c = paths[2].display(),
            outside = paths[3].display(),
            private = paths[5].display(),
            scratch = paths[4].display()
        );
        let script_path = paths[0].join("probe.sh");
        std::fs::write(&script_path, script)?;
        let home = PathBuf::from(std::env::var_os("HOME").ok_or("HOME is unset")?);
        let adapter: Arc<dyn HarnessAdapter> = if claude {
            Arc::new(harness_claude::ClaudeAdapter::with_binary(
                std::env::var_os("OPHIO_LIVE_CLAUDE")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| home.join(".local/bin/claude")),
            ))
        } else {
            Arc::new(harness_codex::CodexAdapter::with_binary(
                std::env::var_os("OPHIO_LIVE_CODEX")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| home.join(".config/ophio/agent-bin/codex")),
            ))
        };
        let spec = RunSpec {
            run_id: RunId::new(),
            task_id: TaskId::new(),
            mode: TaskMode::Work,
            cwd: paths[0].clone(),
            prompt: ports::PromptInput {
                text: format!(
                    "Run bash {} as a single long-running shell command. It is an authorized three-minute sandbox validation using only synthetic data. Let all 18 attempts finish, poll the same command until it completes, and report its complete output verbatim. Do not edit the script, work around the sandbox, or stop early. Expected failures are the test. Do not use other tools. Finish normally after the script completes.",
                    script_path.display()
                ),
                images: vec![],
            },
            model: Some(
                if claude {
                    "claude-haiku-4-5-20251001"
                } else {
                    "gpt-6.1-sol"
                }
                .into(),
            ),
            effort: None,
            launch_policy: Some(LaunchPolicy::BypassPermissions),
            resume_session: None,
            resume_from_mode: None,
            mission: None,
            branch: None,
            start: None,
            bridge: None,
            project_settings: false,
            env: Default::default(),
            github: None,
            scratch: Some(paths[4].clone()),
            questions: false,
            sandbox: Some(sandbox.clone()),
            web: false,
            disabled: Default::default(),
        };
        let mut run = adapter.start_run(spec).await?;
        let pid = run.control.process_id();
        println!(
            "START {} BWRAP_PID={pid:?} ROOT={}",
            now(),
            root.path().display()
        );
        let mut changes = 0;
        let mut finished = false;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(360);
        loop {
            tokio::select! {
                _ = tokio::time::sleep_until(deadline) => { run.control.terminate().await?; return Err("live test timed out".into()); }
                event=run.events.recv() => {
                    let Some(event)=event else { break };
                    match &event {
                        HarnessEvent::SessionStarted { native_session, actual_model, .. } => println!("SESSION {} id={native_session} model={actual_model:?}",now()),
                        HarnessEvent::Activity { tool, detail, summary, .. } => println!("TOOL {} {tool} {summary}\n{}",now(),detail.as_deref().unwrap_or("")),
                        HarnessEvent::AgentMessage { text, .. } => println!("MESSAGE {} {text}",now()),
                        HarnessEvent::Warning { text } => println!("WARNING {text}"),
                        HarnessEvent::ParentEnded { exit } => { println!("TURN_END {} {exit:?}",now()); finished=true; run.control.shutdown().await?; },
                        HarnessEvent::TurnCompleted { .. } => { println!("TURN_COMPLETE {}",now()); finished=true; run.control.shutdown().await?; },
                        HarnessEvent::Exited { exit, .. } => { println!("EXIT {} {exit:?} PID={pid:?} CHANGES={changes}",now()); break; },
                        _ => {}
                    }
                }
                _=tokio::time::sleep(Duration::from_millis(100)) => {
                    let tick=std::fs::read_to_string(paths[4].join("tick")).ok().and_then(|s| s.trim().parse::<u32>().ok());
                    let thresholds=[2,5,8,11,14];
                    if changes<5 && tick.is_some_and(|t| t>=thresholds[changes]) {
                        match changes { 0=>reach.read.push(paths[1].clone()),1=>reach.write.push(paths[2].clone()),2=>reach.internet=true,3=>reach.write.clear(),4=>reach.internet=false,_=>unreachable!() }
                        let applied=sandbox.update(reach.clone()).await;
                        println!("CHANGE {} step={} after_attempt={tick:?} result={applied:?} BWRAP_PID={:?}",now(),changes,run.control.process_id());
                        assert_eq!(applied,ports::sandbox::Applied::Applied);
                        if let Some(pid)=pid { print_tree(pid); }
                        changes+=1;
                    }
                }
            }
        }
        assert!(finished && changes == 5, "turn or grant changes incomplete");
        Ok(())
    }
    fn print_tree(pid: u32) {
        // Only names, PIDs and start times; never command arguments or environment.
        if let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            let end = stat.rfind(')').unwrap();
            let fields: Vec<_> = stat[end + 2..].split_whitespace().collect();
            println!(
                "PROCESS pid={pid} name={} start_ticks={}",
                &stat[stat.find('(').unwrap() + 1..end],
                fields[19]
            );
        }
        if let Ok(children) = std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children")) {
            for child in children.split_whitespace().filter_map(|s| s.parse().ok()) {
                print_tree(child);
            }
        }
    }
}
