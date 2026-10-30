// SPDX-License-Identifier: Apache-2.0
//! Opt-in real companion composition / SetConversationAccess proof. Uses native quota.
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
    use companion::{
        compose::{ComposeOptions, Runtime},
        config::{Config, Paths},
        identity::{FileSecretStore, Identity},
        local::OwnerRuntime,
    };
    use protocol::{access::ConversationAccess, capability::HarnessKind, command::*, model::*, *};
    use services::{Caller, Hub};
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
        let restart = std::env::args().any(|a| a == "--restart");
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
  echo "$i" > "$OPHIO_SCRATCH/tick"
  sleep 10
done
"#,
            b = paths[1].display(),
            c = paths[2].display(),
            outside = paths[3].display(),
            private = paths[5].display()
        );
        let script_path = paths[0].join("probe.sh");
        let script = if restart {
            script
                .replace("seq 0 17", "seq 0 3")
                .replace("sleep 10", "sleep 2")
        } else {
            script
        };
        std::fs::write(&script_path, script)?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&paths[5], std::fs::Permissions::from_mode(0o700))?;
        let app_paths = Paths {
            config: paths[5].join("config"),
            data: paths[5].clone(),
            runtime: paths[5].join("run"),
        };
        app_paths.initialize()?;
        let identity = Identity::load_or_create(
            &FileSecretStore::new(app_paths.identity()),
            vec!["localhost".into()],
        )?;
        let config = Config::default();
        config.save(&app_paths.config_file())?;
        let options = ComposeOptions {
            environment: [("HOME".into(), std::env::var("HOME")?)]
                .into_iter()
                .collect(),
            executable_dir: std::env::current_exe()?
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .to_owned(),
            clock: Arc::new(ports::SystemClock),
            claude_binary: Some(PathBuf::from("/nonexistent/claude")),
            codex_binary: Some(
                std::env::var_os("OPHIO_LIVE_CODEX")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| {
                        PathBuf::from(std::env::var_os("HOME").unwrap())
                            .join(".config/ophio/agent-bin/codex")
                    }),
            ),
            desktop_factory: None,
            speech_factory: Arc::new(companion::frontdoors::LocalSpeech),
            accounts: false,
        };
        let runtime = Runtime::build(app_paths.clone(), config, &identity, options).await?;
        let hub = &runtime.hub;
        let owner = Caller::local_owner();
        println!(
            "CAPABILITY {:?}",
            hub.capabilities().await.conversation_access
        );
        let mut projects = vec![];
        for path in &paths[..3] {
            let receipt = hub
                .submit(
                    &owner,
                    CommandEnvelope::new(Command::AddProject {
                        host_path: path.to_string_lossy().into_owned(),
                        name: None,
                    }),
                )
                .await;
            let Some(CommandResult::ProjectAdded { project_id }) = receipt.result else {
                return Err(format!("{receipt:?}").into());
            };
            projects.push(project_id);
        }
        let mut access = ConversationAccess {
            read: vec![projects[0], projects[2]],
            internet: false,
            ..Default::default()
        };
        let receipt=hub.submit(&owner,CommandEnvelope::new(Command::CreateTask {
        github:None,agent_chosen:None,project_id:projects[0],kind:TaskKind::Coding,mode:Some(TaskMode::Ask),title:Some("Sandbox live proof".into()),
        prompt:format!("Run bash {} as one long-running shell command. This authorized sandbox test uses only synthetic data. Let all scripted attempts finish, polling the same command. Report its complete output verbatim. Do not edit it, work around the sandbox, or stop early. Expected failures are the test. Use no other tools. Finish normally after it completes.",script_path.display()),
        bundle:None,harness:HarnessKind::Codex,model:Some("gpt-6.1-sol".into()),effort:None,launch_policy:None,branch:None,mission:None,target_id:None,
        placement:TaskPlacement::StartNow,start:None,place:None,browser:None,conversation:Some(true),access:Some(access.clone()) , review: None })).await;
        let Some(CommandResult::TaskCreated { task_id, .. }) = receipt.result else {
            return Err(format!("{receipt:?}").into());
        };
        println!(
            "START {} task={task_id} ROOT={}",
            now(),
            root.path().display()
        );
        let id = task_id.as_uuid().simple().to_string();
        let scratch = app_paths
            .database()
            .join("scratch")
            .join(&id[id.len() - 12..]);
        let mut changes = 0;
        let mut seq = 0;
        let mut agent_pid = None;
        let mut initial_run = None;
        let mut previous_block = None;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(360);
        loop {
            if tokio::time::Instant::now() > deadline {
                runtime.shutdown().await?;
                return Err("companion live test timed out".into());
            }
            if let QueryData::TaskTimeline { mut items, .. } = hub
                .query(
                    &owner,
                    Query::TaskTimeline {
                        task_id,
                        before_seq: None,
                        limit: 1000,
                    },
                )
                .await?
            {
                items.sort_by_key(|i| i.seq);
                for item in items {
                    if item.seq > seq {
                        println!("TIMELINE {} {} {:?}", now(), item.seq, item.item);
                        seq = item.seq;
                    }
                }
            }
            let QueryData::Task { records } = hub.query(&owner, Query::Task { task_id }).await?
            else {
                unreachable!()
            };
            let task = records.tasks.iter().find(|t| t.task_id == task_id).unwrap();
            if initial_run.is_none() {
                initial_run = task.current_run;
            }
            if !restart {
                assert_eq!(
                    task.current_run, initial_run,
                    "grant update restarted the native turn"
                );
            }
            if task.access_blocked != previous_block {
                println!("BLOCK {} {:?}", now(), task.access_blocked);
                previous_block = task.access_blocked.clone();
            }
            let tick = std::fs::read_to_string(scratch.join("tick"))
                .ok()
                .and_then(|s| s.trim().parse::<u32>().ok());
            let thresholds = if restart {
                [1, 5, 8, 11, 14]
            } else {
                [2, 5, 8, 11, 14]
            };
            if changes < if restart { 1 } else { 5 }
                && tick.is_some_and(|t| t >= thresholds[changes])
            {
                if restart {
                    println!("RESTART_OLD_PID {:?}", child_bwrap());
                    access.read.retain(|p| *p != projects[2]);
                } else {
                    match changes {
                        0 => access.read.push(projects[1]),
                        1 => access.write.push(projects[2]),
                        2 => access.internet = true,
                        3 => access.write.clear(),
                        4 => access.internet = false,
                        _ => unreachable!(),
                    }
                }
                let command = CommandEnvelope::new(Command::SetConversationAccess {
                    task_id,
                    access: access.clone(),
                });
                let command_id = command.command_id;
                let receipt = hub.submit(&owner, command).await;
                assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
                tokio::time::timeout(Duration::from_secs(10), async {
                    loop {
                        if let QueryData::CommandStatus { stage, error, .. } = hub
                            .query(&owner, Query::CommandStatus { command_id })
                            .await
                            .unwrap()
                        {
                            assert!(error.is_none(), "{error:?}");
                            if stage == Some(CommandStage::Observed) {
                                break;
                            }
                        }
                        tokio::time::sleep(Duration::from_millis(20)).await;
                    }
                })
                .await?;
                if agent_pid.is_none() {
                    agent_pid = child_bwrap();
                }
                println!(
                    "CHANGE {} step={} after_attempt={tick:?} OBSERVED RUN={initial_run:?} PID={agent_pid:?}",
                    now(),
                    changes
                );
                print_tree(agent_pid.expect("live bwrap child"));
                changes += 1;
            }
            if matches!(
                task.state,
                TaskState::Completed | TaskState::Failed | TaskState::Interrupted
            ) {
                for run in &records.runs {
                    println!(
                        "NATIVE run={} session={:?} model={:?}",
                        run.run_id, run.native_session, run.actual_model
                    );
                    assert_eq!(run.actual_model.as_deref(), Some("gpt-6.1-sol"));
                }
                println!(
                    "END {} state={:?} RUN={initial_run:?} PID={agent_pid:?} CHANGES={changes}",
                    now(),
                    task.state
                );
                assert_eq!(task.state, TaskState::Completed);
                assert_eq!(changes, if restart { 1 } else { 5 });
                if restart {
                    assert_eq!(records.runs.len(), 2);
                    let first = records.runs[0].native_session.as_ref().unwrap();
                    assert!(
                        records
                            .runs
                            .iter()
                            .all(|run| run.native_session.as_ref() == Some(first))
                    );
                    println!("RESTART_VERIFIED same_native_session={first}");
                }
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        runtime.shutdown().await?;
        Ok(())
    }
    fn child_bwrap() -> Option<u32> {
        for entry in std::fs::read_dir("/proc").ok()?.flatten() {
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|s| s.parse::<u32>().ok())
            else {
                continue;
            };
            let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
                continue;
            };
            if let Some(tail) = stat.split_once("(bwrap) ").map(|(_, s)| s)
                && tail
                    .split_whitespace()
                    .nth(1)
                    .and_then(|s| s.parse::<u32>().ok())
                    == Some(std::process::id())
            {
                return Some(pid);
            }
        }
        None
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
