// SPDX-License-Identifier: Apache-2.0
//! Upgrading an install made before the product was named, with the real
//! binary, a temporary home and a stand-in `systemctl` that only records calls.
#![cfg(target_os = "linux")]
use companion::{
    config::{Config, Paths},
    identity::{FileSecretStore, Identity},
};
use std::{path::PathBuf, process::Output, time::Duration};

const EARLIER_UNIT: &str = "# Managed by companion; remove with the service uninstall command.\n[Unit]\nDescription=Host companion\nAfter=graphical-session.target\nPartOf=graphical-session.target\n\n[Service]\nType=simple\nExecStart=\"/home/sam/.local/opt/companion/bin/companion\" run\nRestart=on-failure\nRestartSec=3\nUMask=0077\n\n[Install]\nWantedBy=graphical-session.target\n";

struct Home {
    temp: tempfile::TempDir,
}

impl Home {
    /// An earlier install: settings, data with its identity, and nothing else.
    fn with_earlier_install() -> (Self, Identity) {
        let home = Self {
            temp: tempfile::tempdir().unwrap(),
        };
        let bin = home.path("bin");
        std::fs::create_dir(&bin).unwrap();
        for name in ["claude", "codex", "tailscale"] {
            test_support::executable(bin.join(name), "#!/bin/sh\nexit 1\n").unwrap();
        }
        test_support::executable(
            bin.join("systemctl"),
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$0.log\"\n",
        )
        .unwrap();
        let earlier = home.paths("companion");
        earlier.initialize().unwrap();
        Config {
            port: 0,
            retention_days: 9,
            ..Default::default()
        }
        .save(&earlier.config_file())
        .unwrap();
        let identity = Identity::load_or_create(
            &FileSecretStore::new(earlier.identity()),
            vec!["localhost".into()],
        )
        .unwrap();
        (home, identity)
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.temp.path().join(relative)
    }

    fn paths(&self, name: &str) -> Paths {
        Paths {
            config: self.path("config").join(name),
            data: self.path("data").join(name),
            runtime: self.path("runtime").join(name).join("run"),
        }
    }

    fn units(&self) -> PathBuf {
        self.path("config/systemd/user")
    }

    fn command(&self) -> tokio::process::Command {
        let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_ophio"));
        command
            .env_clear()
            .env("PATH", self.path("bin"))
            .env("HOME", self.path("home"))
            .env("SHELL", "/bin/sh")
            .env("XDG_CONFIG_HOME", self.path("config"))
            .env("XDG_DATA_HOME", self.path("data"))
            .env("XDG_RUNTIME_DIR", self.path("runtime"))
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true);
        command
    }

    async fn ok(&self, args: &[&str]) -> Output {
        let output = self.command().args(args).output().await.unwrap();
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    fn systemctl_calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.path("bin/systemctl.log"))
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

#[tokio::test]
async fn an_earlier_install_upgrades_with_the_same_identity() {
    let (home, identity) = Home::with_earlier_install();
    std::fs::create_dir_all(home.units()).unwrap();
    let earlier_unit = home.units().join("companion.service");
    std::fs::write(&earlier_unit, EARLIER_UNIT).unwrap();

    home.ok(&["service", "install"]).await;
    assert!(!earlier_unit.exists());
    let unit = std::fs::read_to_string(home.units().join("ophio.service")).unwrap();
    assert!(unit.starts_with("# Managed by ophio;"), "{unit}");
    assert!(unit.contains(&format!(
        "ExecStart=\"{}\" run",
        env!("CARGO_BIN_EXE_ophio")
    )));
    assert_eq!(
        home.systemctl_calls(),
        [
            "--user is-active graphical-session.target",
            "--user disable --now companion.service",
            "--user daemon-reload",
            "--user enable --now ophio.service",
        ]
    );

    // What the new unit starts: the first run moves settings and data.
    let mut running = home
        .command()
        .arg("run")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let status = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let output = home
                .command()
                .args(["status", "--json"])
                .output()
                .await
                .unwrap();
            if output.status.success() {
                let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
                if value.get("snapshot").is_some() {
                    break value;
                }
            }
            assert!(running.try_wait().unwrap().is_none(), "ophio run exited");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        status["snapshot"]["host"]["host_id"],
        identity.host_id.to_string()
    );
    running.kill().await.unwrap();

    let (earlier, current) = (home.paths("companion"), home.paths("ophio"));
    assert!(!earlier.config.exists() && !earlier.data.exists());
    assert_eq!(
        Config::load(&current.config_file()).unwrap().retention_days,
        9
    );
    let moved =
        Identity::load_or_create(&FileSecretStore::new(current.identity()), vec![]).unwrap();
    assert_eq!(moved.host_id, identity.host_id);
    assert_eq!(moved.fingerprint(), identity.fingerprint());
    assert!(current.database().exists());
}

#[tokio::test]
async fn an_existing_new_directory_and_an_unmanaged_unit_are_left_alone() {
    let (home, identity) = Home::with_earlier_install();
    let current = home.paths("ophio");
    companion::identity::private_dir(&current.config).unwrap();
    Config {
        port: 0,
        retention_days: 5,
        ..Default::default()
    }
    .save(&current.config_file())
    .unwrap();
    std::fs::create_dir_all(home.units()).unwrap();
    let other_unit = home.units().join("companion.service");
    std::fs::write(
        &other_unit,
        "[Unit]\nDescription=Someone else's companion\n",
    )
    .unwrap();

    home.ok(&["service", "install"]).await;
    assert_eq!(
        std::fs::read_to_string(&other_unit).unwrap(),
        "[Unit]\nDescription=Someone else's companion\n"
    );
    assert!(
        home.systemctl_calls()
            .iter()
            .all(|call| !call.contains("companion.service"))
    );

    let output = home.ok(&["status", "--json"]).await;
    assert!(output.stderr.is_empty(), "{output:?}");
    let earlier = home.paths("companion");
    assert_eq!(
        Config::load(&earlier.config_file()).unwrap().retention_days,
        9
    );
    assert_eq!(
        Config::load(&current.config_file()).unwrap().retention_days,
        5
    );
    // The data directory had no new counterpart, so it moved.
    assert!(!earlier.data.exists());
    let moved =
        Identity::load_or_create(&FileSecretStore::new(current.identity()), vec![]).unwrap();
    assert_eq!(moved.host_id, identity.host_id);
}
