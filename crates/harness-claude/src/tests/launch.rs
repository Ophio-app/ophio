// SPDX-License-Identifier: Apache-2.0
use super::*;

#[test]
fn launch_flags_are_explicit_and_read_only_modes_override_them() {
    let (_dir, _adapter, mut spec) = fixture();
    for (policy, permission) in [
        (LaunchPolicy::Standard, "manual"),
        (LaunchPolicy::BypassPermissions, "bypassPermissions"),
        (LaunchPolicy::AcceptEdits, "acceptEdits"),
    ] {
        spec.launch_policy = Some(policy);
        for mode in [TaskMode::Work, TaskMode::Ask, TaskMode::Plan] {
            spec.mode = mode;
            let launch = Launch::build(&spec).unwrap();
            let args = &launch.args;
            let at = args.iter().position(|a| a == "--permission-mode").unwrap();
            assert_eq!(
                args[at + 1],
                if mode == TaskMode::Work {
                    permission
                } else {
                    "plan"
                }
            );
            assert_eq!(
                args.iter().any(|a| a == "--dangerously-skip-permissions"),
                mode == TaskMode::Work && policy == LaunchPolicy::BypassPermissions
            );
            assert!(args.windows(2).any(|a| a == ["--resume", "old-session"]));
            if mode != TaskMode::Work {
                assert!(args.windows(2).any(|a| a == ["--tools", "Read,Glob,Grep"]));
                assert!(args.iter().any(|a| a == "--safe-mode"));
            }
        }
    }
    spec.mode = TaskMode::Work;
    spec.launch_policy = Some(LaunchPolicy::Sandboxed);
    assert!(Launch::build(&spec).is_err());
    spec.launch_policy = Some(LaunchPolicy::BypassPermissions);
    spec.bridge.as_mut().unwrap().computer_use = true;
    assert!(Launch::build(&spec).is_err());
}

#[tokio::test]
async fn selected_bypass_reaches_native_launch_and_old_versions_refuse() {
    for supported in [true, false] {
        let (dir, adapter, mut spec) = fixture();
        spec.launch_policy = Some(LaunchPolicy::BypassPermissions);
        spec.prompt.text = "wait".into();
        if !supported {
            std::fs::write(dir.path().join("no-bypass"), "").unwrap();
        }
        let result = adapter.start_run(spec).await;
        if supported {
            let mut run = result.unwrap();
            assert!(matches!(
                event(&mut run.events).await,
                HarnessEvent::SessionStarted { .. }
            ));
            let args: Vec<String> =
                serde_json::from_slice(&std::fs::read(dir.path().join("argv.json")).unwrap())
                    .unwrap();
            assert!(args.iter().any(|a| a == "--dangerously-skip-permissions"));
            run.control.terminate().await.unwrap();
        } else {
            assert!(matches!(result, Err(HarnessError::Unsupported(_))));
            assert!(!dir.path().join("argv.json").exists());
        }
    }
}
