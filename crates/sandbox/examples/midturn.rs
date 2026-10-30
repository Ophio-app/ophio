// SPDX-License-Identifier: Apache-2.0
//! Reproducible namespace smoke test. Run from a user service on restricted hosts.
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
    use ports::sandbox::{Reach, SandboxSpec, Sandboxes, WrapRequest};
    use protocol::capability::HarnessKind;
    use std::{path::PathBuf, process::Stdio};

    #[tokio::main]
    pub async fn main() -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let paths: Vec<_> = ["A", "B", "C", "outside", "config"]
            .iter()
            .map(|p| root.path().join(p))
            .collect();
        for p in &paths {
            std::fs::create_dir(p)?;
        }
        std::fs::write(paths[1].join("note.txt"), "B sentinel")?;
        std::fs::write(paths[3].join("secret.txt"), "outside sentinel")?;
        let shim = std::env::current_exe()?
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("ophio-sandbox-shim");
        let backend = sandbox::Bubblewrap::new(shim);
        println!("probe={:?}", backend.availability());
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
            harness: HarnessKind::Codex,
            cwd: paths[0].clone(),
            state: root.path().join("state"),
            always: vec![],
            programs: vec![],
            candidates: paths[..3].to_vec(),
            private: vec![],
            given: vec![],
            relay: None,
            on_blocked: None,
        })?;
        let env = ports::harness::RunEnv(vec![(
            "CODEX_HOME".into(),
            paths[4].to_string_lossy().into_owned(),
        )]);
        let program = PathBuf::from("/bin/sh");
        let wrapped = sandbox.wrap(WrapRequest {
            program: &program,
            visible: &[],
            env: &env,
            resume: None,
        })?;
        let script = format!(
            "i=0; while [ $i -lt 8 ]; do date -u +%T; echo pid=$$; cat '{}/note.txt'; echo test > '{}/out-'$i; cat '{}/secret.txt'; i=$((i+1)); sleep 1; done",
            paths[1].display(),
            paths[2].display(),
            paths[3].display()
        );
        let mut command = tokio::process::Command::new(&wrapped.program);
        command
            .args(wrapped.args)
            .arg(&program)
            .args(["-c", &script])
            .envs(env.0)
            .envs(wrapped.env)
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        sandbox.prepare(command.as_std_mut())?;
        let mut child = command.spawn()?;
        println!("bwrap_pid={:?}", child.id());
        sandbox.started().await?;
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        reach.read.push(paths[1].clone());
        reach.write.push(paths[2].clone());
        reach.internet = true;
        println!("grant={:?}", sandbox.update(reach.clone()).await);
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        reach.write.clear();
        reach.internet = false;
        println!("revoke={:?}", sandbox.update(reach).await);
        println!("exit={}", child.wait().await?);
        sandbox.finished().await?;
        Ok(())
    }
}
