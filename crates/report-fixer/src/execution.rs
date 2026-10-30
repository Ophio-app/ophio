// SPDX-License-Identifier: Apache-2.0
use crate::{
    config::{AgentCli, Config, Provider},
    repository, *,
};
use ports::{RunEnv, sandbox::*};
use protocol::{capability::HarnessKind, time::Timestamp};
use report_pipeline::{gates::TestResult, schema::TestKind, signals::Signal};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex},
    time::{Duration, UNIX_EPOCH},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Limits {
    pub wall: Duration,
    pub cpu_seconds: u64,
    pub memory_bytes: u64,
    pub output_bytes: usize,
    pub disk_bytes: u64,
    pub disk_entries: u64,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            wall: Duration::from_secs(600),
            cpu_seconds: 600,
            memory_bytes: 8 * 1024 * 1024 * 1024,
            output_bytes: 8 * 1024 * 1024,
            disk_bytes: 16 * 1024 * 1024 * 1024,
            disk_entries: 250_000,
        }
    }
}
pub fn readiness(config: &Config) -> Result<()> {
    if !sandbox::Bubblewrap::new(&config.shim)
        .availability()
        .is_available()
    {
        return Err(Error::Operation("sandbox user namespaces are unavailable"));
    }
    let _container = containment::Container::limited(containment::ResourceLimits {
        memory_bytes: config.limits.memory_bytes,
        cpu_millis_per_second: 4000,
        processes: 256,
    })
    .map_err(|_| {
        Error::Operation("delegated CPU, memory and process controllers are unavailable")
    })?;
    Ok(())
}
#[derive(Debug)]
pub struct SandboxedAgents {
    pub test_upstream: Option<std::net::SocketAddr>,
    pub config: Config,
    pub limits: Limits,
}
fn harness(cli: AgentCli) -> HarnessKind {
    match cli {
        AgentCli::Codex => HarnessKind::Codex,
        AgentCli::Claude => HarnessKind::ClaudeCode,
    }
}
fn reach(workspace: &Path, writable: bool) -> Reach {
    Reach {
        read: if writable {
            vec![]
        } else {
            vec![workspace.into()]
        },
        write: if writable {
            vec![workspace.into()]
        } else {
            vec![]
        },
        whole_read: false,
        whole_write: false,
        internet: false,
        local_network: false,
    }
}
fn spec(workspace: &Path, state: PathBuf, harness: HarnessKind, writable: bool) -> SandboxSpec {
    SandboxSpec {
        offline: false,
        reach: reach(workspace, writable),
        harness,
        cwd: workspace.into(),
        state,
        always: vec![],
        programs: vec![],
        candidates: vec![],
        private: vec![],
        given: vec![],
        on_blocked: None,
        relay: None,
    }
}
#[derive(Debug)]
struct Output {
    success: bool,
    /// The exit code; a signal shows as 128 plus its number through the
    /// sandbox, or as none when this process saw the signal itself.
    code: Option<i32>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}
async fn bounded_read(
    mut reader: impl tokio::io::AsyncRead + Unpin,
    limit: usize,
) -> Result<Vec<u8>> {
    let mut bytes = vec![];
    (&mut reader)
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .await?;
    if bytes.len() > limit {
        return Err(Error::OutputCap);
    }
    Ok(bytes)
}
struct RunBudget<'a> {
    limits: &'a Limits,
    disk: Arc<sandbox::DiskBudget>,
    deadline_ms: i64,
}
async fn disk_check(disk: Arc<sandbox::DiskBudget>) -> Result<()> {
    if tokio::task::spawn_blocking(move || disk.exceeded())
        .await
        .map_err(|_| Error::Signal(Signal::BudgetSpent))?
        .unwrap_or(true)
    {
        return Err(Error::Signal(Signal::BudgetSpent));
    }
    Ok(())
}
async fn disk_watch(disk: Arc<sandbox::DiskBudget>) -> Result<Output> {
    loop {
        disk_check(disk.clone()).await?;
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
async fn execute(
    sandbox: Arc<dyn Sandbox>,
    program: &Path,
    args: &[String],
    env: RunEnv,
    stdin: &[u8],
    budget: RunBudget<'_>,
) -> Result<Output> {
    let RunBudget {
        limits,
        disk,
        deadline_ms,
    } = budget;
    let container = Arc::new(
        containment::Container::limited(containment::ResourceLimits {
            memory_bytes: limits.memory_bytes,
            cpu_millis_per_second: 4000,
            processes: 256,
        })
        .map_err(|_| Error::Operation("aggregate resource caps unavailable"))?,
    );
    let guard = containment::Guard::new(container.clone());
    let wrapped = sandbox
        .wrap(WrapRequest {
            program,
            visible: &[],
            env: &env,
            resume: None,
        })
        .map_err(|_| Error::Operation("sandbox command"))?;
    disk_check(disk.clone()).await?;
    let mut command = tokio::process::Command::new(wrapped.program);
    command
        .env_clear()
        .args(wrapped.args)
        .arg("/usr/bin/prlimit")
        .arg(format!("--cpu={}", limits.cpu_seconds))
        .arg(format!("--as={}", limits.memory_bytes))
        .args(["--fsize=134217728", "--"])
        .arg(program)
        .args(args)
        .envs(env.0)
        .envs(wrapped.env)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    sandbox.prepare(command.as_std_mut())?;
    let mut child = container
        .spawn(&mut command)
        .await
        .map_err(|_| Error::Operation("contained process start"))?;
    sandbox
        .started()
        .await
        .map_err(|_| Error::Operation("sandbox start"))?;
    let mut input = child
        .stdin
        .take()
        .ok_or(Error::Operation("process stdin"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or(Error::Operation("process stdout"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or(Error::Operation("process stderr"))?;
    let wall = Duration::from_millis(deadline_ms.saturating_sub(Timestamp::now().0).max(0) as u64)
        .min(limits.wall);
    let work = async {
        let writer = async {
            input.write_all(stdin).await?;
            drop(input);
            Ok::<_, Error>(())
        };
        let (status, out, err, _) = tokio::try_join!(
            async { Ok::<_, Error>(child.wait().await?) },
            bounded_read(stdout, limits.output_bytes),
            bounded_read(stderr, limits.output_bytes),
            writer
        )?;
        Ok::<_, Error>(Output {
            success: status.success(),
            code: status.code(),
            stdout: out,
            stderr: err,
        })
    };
    let checked = async {
        tokio::select! {
            result = disk_watch(disk.clone()) => result,
            result = work => result,
        }
    };
    let out = match tokio::time::timeout(wall, checked).await {
        Ok(result) => result,
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            Err(Error::TimedOut)
        }
    };
    guard
        .end()
        .await
        .map_err(|_| Error::Operation("process tree did not stop"))?;
    let _ = child.wait().await;
    // The pid namespace ends with bwrap's init; nothing is reused.
    sandbox
        .finished()
        .await
        .map_err(|_| Error::Operation("sandbox cleanup"))?;
    disk_check(disk).await?;
    out
}
#[async_trait::async_trait]
impl AgentRunner for SandboxedAgents {
    fn watch_credentials(
        &self,
        canaries: &mut report_pipeline::honeytoken::Honeytokens,
    ) -> Result<()> {
        use std::io::Read;
        for path in [
            &self.config.intake_token_file,
            &self.config.provider_key_file,
        ] {
            let mut text = String::new();
            std::fs::File::open(path)?
                .take(16385)
                .read_to_string(&mut text)?;
            if text.trim().is_empty() || text.len() > 16384 {
                return Err(Error::Operation("credential file is empty or too large"));
            }
            canaries.watch(text.trim().as_bytes());
        }
        Ok(())
    }
    async fn run(&self, job: &AgentJob) -> Result<AgentOutput> {
        let stage = tempfile::tempdir_in(&self.config.state)?;
        let signals = Arc::new(Mutex::new(Vec::new()));
        let observed = signals.clone();
        let scanned = signals.clone();
        let mut canaries = job.canaries.as_ref().clone();
        self.watch_credentials(&mut canaries)?;
        let canaries = Arc::new(canaries);
        let relay_canaries = canaries.clone();
        let model = match job.prompt.stage() {
            AgentStage::Triage => &self.config.triage_model,
            AgentStage::Fix => &self.config.fix_model,
            AgentStage::Review => &self.config.review_model,
        };
        let mut spec = spec(
            &job.workspace,
            stage.path().join("state"),
            harness(self.config.agent_cli),
            job.prompt.stage() == AgentStage::Fix,
        );
        spec.always = vec![
            job.home.join(".ssh"),
            job.home.join(".config/claude"),
            job.output
                .parent()
                .ok_or(Error::Operation("output directory"))?
                .into(),
        ];
        spec.programs.push(self.config.agent_program.clone());
        let audit_directory = self.config.state.join("relay");
        std::fs::create_dir_all(&audit_directory)?;
        let audit_file = tempfile::NamedTempFile::new_in(&audit_directory)?;
        let (_, audit_path) = audit_file
            .keep()
            .map_err(|_| Error::Operation("relay audit file"))?;
        spec.relay = Some(RelaySpec {
            provider: match self.config.provider {
                Provider::Openai => RelayProvider::OpenAi,
                Provider::Anthropic => RelayProvider::Anthropic,
            },
            key_file: self.config.provider_key_file.clone(),
            budget: RelayBudget {
                max_requests: 64,
                max_input_tokens: job.input_limit,
                max_output_tokens: job.output_limit,
                deadline: Some(UNIX_EPOCH + Duration::from_millis(job.deadline_ms.max(0) as u64)),
            },
            policy: RelayPolicy {
                allowed_models: Some(vec![model.clone()]),
                max_output_tokens: Some(job.output_limit),
            },
            log: audit_path,
            test_upstream: self.test_upstream,
            on_signal: Some(Arc::new(move |s| {
                observed.lock().unwrap().push(match s {
                    RelaySignal::HijackAttempt => Signal::RelayRefusal,
                    RelaySignal::BudgetSpent => Signal::BudgetSpent,
                });
            })),
            inspect: Some(Arc::new(move |bytes| {
                let seen = relay_canaries.scan(bytes).is_some();
                if seen {
                    scanned.lock().unwrap().insert(0, Signal::CanarySeen);
                }
                seen
            })),
        });
        let disk = Arc::new(sandbox::DiskBudget::new(
            self.limits.disk_bytes,
            self.limits.disk_entries,
        )?);
        let sandbox = sandbox::Bubblewrap::new(&self.config.shim)
            .with_disk_budget(disk.clone())
            .create(spec)
            .map_err(|_| Error::Operation("sandbox unavailable"))?;
        let endpoint = sandbox.relay().ok_or(Error::Operation("relay missing"))?;
        let mut env = RunEnv(vec![(
            "HOME".into(),
            job.home.to_string_lossy().into_owned(),
        )]);
        env.0.push((
            "OPHIO_REPORT_OUTPUT_DIR".into(),
            job.output
                .parent()
                .ok_or(Error::Operation("output directory"))?
                .to_string_lossy()
                .into_owned(),
        ));
        let args = match self.config.agent_cli {
            AgentCli::Codex => {
                env.0.push((
                    "OPHIO_RELAY_DUMMY_KEY".into(),
                    "invalid-relay-placeholder".into(),
                ));
                vec![
                    "exec".into(),
                    "--skip-git-repo-check".into(),
                    "--dangerously-bypass-approvals-and-sandbox".into(),
                    "--model".into(),
                    model.clone(),
                    "--output-last-message".into(),
                    job.output.to_string_lossy().into_owned(),
                    "-c".into(),
                    "forced_login_method=\"api\"".into(),
                    "-c".into(),
                    "model_provider=\"ophio_relay\"".into(),
                    "-c".into(),
                    "model_providers.ophio_relay.name=\"Ophio relay\"".into(),
                    "-c".into(),
                    format!(
                        "model_providers.ophio_relay.base_url=\"{}/v1\"",
                        endpoint.base_url()
                    ),
                    "-c".into(),
                    "model_providers.ophio_relay.wire_api=\"responses\"".into(),
                    "-c".into(),
                    "model_providers.ophio_relay.env_key=\"OPHIO_RELAY_DUMMY_KEY\"".into(),
                    "-c".into(),
                    "model_providers.ophio_relay.requires_openai_auth=false".into(),
                    "-c".into(),
                    "model_providers.ophio_relay.supports_websockets=false".into(),
                    "-c".into(),
                    "analytics.enabled=false".into(),
                    "-".into(),
                ]
            }
            AgentCli::Claude => {
                env.0.extend([
                    ("ANTHROPIC_BASE_URL".into(), endpoint.base_url()),
                    (
                        "ANTHROPIC_API_KEY".into(),
                        "invalid-relay-placeholder".into(),
                    ),
                    (
                        "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC".into(),
                        "1".into(),
                    ),
                ]);
                vec![
                    "-p".into(),
                    "--dangerously-skip-permissions".into(),
                    "--model".into(),
                    model.clone(),
                    "--output-format".into(),
                    "text".into(),
                ]
            }
        };
        let execution = execute(
            sandbox.clone(),
            &self.config.agent_program,
            &args,
            env,
            job.prompt.text().as_bytes(),
            RunBudget {
                limits: &self.limits,
                disk,
                deadline_ms: job.deadline_ms,
            },
        )
        .await;
        let out = match execution {
            Ok(out) => out,
            Err(error) => {
                let seen = signals.lock().unwrap().clone();
                if seen.contains(&Signal::CanarySeen) {
                    return Ok(AgentOutput {
                        raw: vec![],
                        totals: sandbox.relay_totals().unwrap_or_default(),
                        signals: seen,
                    });
                }
                return Err(error);
            }
        };
        if canaries.scan(&out.stdout).is_some() || canaries.scan(&out.stderr).is_some() {
            signals.lock().unwrap().insert(0, Signal::CanarySeen);
        }
        let totals = sandbox
            .relay_totals()
            .ok_or(Error::Operation("relay totals missing"))?;
        if !out.success && signals.lock().unwrap().is_empty() {
            return Err(Error::Operation("agent failed"));
        }
        let raw = if self.config.agent_cli == AgentCli::Codex {
            match std::fs::symlink_metadata(&job.output) {
                // A stopped agent may never write its last message.
                Err(error) if error.kind() == std::io::ErrorKind::NotFound && !out.success => {
                    vec![]
                }
                metadata => {
                    let m = metadata?;
                    if !m.is_file() || m.file_type().is_symlink() || m.len() > 4096 {
                        return Err(Error::Signal(Signal::OffSchemaOutput));
                    }
                    std::fs::read(&job.output)?
                }
            }
        } else {
            out.stdout
        };
        if canaries.scan(&raw).is_some() {
            signals.lock().unwrap().insert(0, Signal::CanarySeen);
        }
        let signals = signals.lock().unwrap().clone();
        Ok(AgentOutput {
            raw,
            totals,
            signals,
        })
    }
}

#[derive(Debug)]
pub struct SandboxedTests {
    pub shim: PathBuf,
    pub scratch: PathBuf,
    pub cargo_cache: PathBuf,
    pub npm_cache: PathBuf,
    pub limits: Limits,
}
impl SandboxedTests {
    async fn command(
        &self,
        workspace: &Path,
        program: &str,
        args: Vec<String>,
        deadline_ms: i64,
    ) -> Result<Output> {
        let state = tempfile::tempdir_in(&self.scratch)?;
        let home = state.path().join("home");
        let target_dir = state.path().join("target");
        let cargo_home = home.join("cargo");
        let npm_home = home.join("npm");
        for path in [&home, &target_dir, &cargo_home, &npm_home] {
            std::fs::create_dir_all(path)?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            for name in ["registry", "git"] {
                if self.cargo_cache.join(name).exists() {
                    symlink(self.cargo_cache.join(name), cargo_home.join(name))?;
                }
            }
            let modules = workspace.join("client/node_modules");
            if workspace.join("client").is_dir()
                && !modules.exists()
                && self.npm_cache.join("node_modules").is_dir()
            {
                symlink(self.npm_cache.join("node_modules"), modules)?;
            }
        }
        let mut spec = spec(
            workspace,
            state.path().join("state"),
            HarnessKind::Codex,
            false,
        );
        spec.offline = true;
        spec.always.extend([home.clone(), target_dir.clone()]);
        spec.reach
            .read
            .extend([self.cargo_cache.clone(), self.npm_cache.clone()]);
        let disk = Arc::new(sandbox::DiskBudget::new(
            self.limits.disk_bytes,
            self.limits.disk_entries,
        )?);
        let sandbox = sandbox::Bubblewrap::new(&self.shim)
            .with_disk_budget(disk.clone())
            .create(spec)
            .map_err(|_| Error::Operation("test sandbox unavailable"))?;
        if sandbox.relay().is_some() {
            return Err(Error::Operation("test network forbidden"));
        }
        let env = RunEnv(vec![
            ("HOME".into(), home.to_string_lossy().into_owned()),
            ("PATH".into(), "/usr/bin:/bin".into()),
            (
                "CARGO_HOME".into(),
                cargo_home.to_string_lossy().into_owned(),
            ),
            ("CARGO_NET_OFFLINE".into(), "true".into()),
            (
                "CARGO_TARGET_DIR".into(),
                target_dir.to_string_lossy().into_owned(),
            ),
            ("CARGO_BUILD_JOBS".into(), "4".into()),
            ("npm_config_offline".into(), "true".into()),
            ("npm_config_ignore_scripts".into(), "true".into()),
            // Backtraces let a check name where a failure happened in our tree.
            ("RUST_BACKTRACE".into(), "1".into()),
            (
                "npm_config_cache".into(),
                npm_home.to_string_lossy().into_owned(),
            ),
        ]);
        execute(
            sandbox,
            Path::new(program),
            &args,
            env,
            b"",
            RunBudget {
                limits: &self.limits,
                disk,
                deadline_ms,
            },
        )
        .await
    }
    /// A Rust repro's result from one contained `sh` that builds it with
    /// `--no-run`, prints `marker:<status>` on stdout, and only then runs it:
    /// so a build failure is never confused with a run that died. The test
    /// writes its own output after the marker; only the first one counts.
    /// `Failed` needs libtest's own shape for an assertion panic: a
    /// `thread '…' panicked at` line directly followed by `assertion failed:`
    /// or `assertion `left == right` failed` (or `!=`). The test controls
    /// its output, so this is what the run reported, not proof of a bug.
    fn rust(out: &Output, marker: &str) -> (TestResult, Option<RunDetail>) {
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        let offline = stderr.contains("--offline was specified")
            || stderr.contains("you're using offline mode");
        let resources = Self::resource_death(out, &stderr);
        let built = stdout
            .split_once(&format!("{marker}:"))
            .map(|(_, rest)| (rest.lines().next().unwrap_or("").trim() == "0", rest));
        let Some((true, run)) = built else {
            let detail = if offline && !stderr.contains("could not compile") {
                Some(RunDetail::MissingDependencies)
            } else if resources {
                Some(RunDetail::OverLimits)
            } else {
                None
            };
            return (TestResult::CompileError, detail);
        };
        let passed = |prefix: &str| {
            run.lines()
                .filter_map(|l| l.strip_prefix(prefix))
                .filter_map(|s| s.split_whitespace().next()?.parse::<u64>().ok())
                .collect::<Vec<_>>()
        };
        let ok = passed("test result: ok. ");
        let failed = run.lines().any(|l| l.starts_with("test result: FAILED."));
        if out.success && ok.iter().any(|n| *n > 0) && !failed {
            (TestResult::Passed, None)
        } else if !out.success && failed && Self::assertion_panic(&stderr) {
            (TestResult::Failed, None)
        } else if !out.success && failed {
            (
                TestResult::CompileError,
                Some(RunDetail::FailedWithoutAssertion),
            )
        } else if out.success && !ok.is_empty() && !failed {
            (TestResult::CompileError, Some(RunDetail::NoTestRan))
        } else if resources {
            (TestResult::CompileError, Some(RunDetail::OverLimits))
        } else {
            (TestResult::CompileError, Some(RunDetail::Crashed))
        }
    }
    fn assertion_panic(stderr: &str) -> bool {
        let lines: Vec<_> = stderr.lines().collect();
        lines.windows(2).any(|pair| {
            pair[0].starts_with("thread '")
                && pair[0].contains("' panicked at ")
                && (pair[1].starts_with("assertion failed: ")
                    || pair[1].starts_with("assertion `left == right` failed")
                    || pair[1].starts_with("assertion `left != right` failed"))
        })
    }
    /// Killed for memory, CPU time, file size or by the cgroup, as far as
    /// the exit status and cargo's report of its child's signal tell.
    fn resource_death(out: &Output, stderr: &str) -> bool {
        // SIGKILL, SIGXCPU and SIGXFSZ, through the sandbox's exit status.
        matches!(out.code, Some(137 | 152 | 153))
            || [
                "signal: 9, SIGKILL",
                "SIGXCPU",
                "SIGXFSZ",
                "memory allocation of ",
            ]
            .iter()
            .any(|s| stderr.contains(s))
    }
    /// A Vitest JSON report's result, what it says beyond that, and the
    /// failing tests' messages, where their stacks are. `Failed` needs a
    /// failing test whose message starts with Vitest's `AssertionError:`.
    fn vitest(out: &Output, json: &serde_json::Value) -> (TestResult, Option<RunDetail>, String) {
        let count = |field: &str| json[field].as_u64().unwrap_or(0);
        let (passed, failed) = (count("numPassedTests"), count("numFailedTests"));
        let suites = json["testResults"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let messages: Vec<&str> = suites
            .iter()
            .filter_map(|suite| suite["assertionResults"].as_array())
            .flatten()
            .filter(|test| test["status"] == "failed")
            .filter_map(|test| test["failureMessages"].as_array())
            .flatten()
            .filter_map(|s| s.as_str())
            .collect();
        let text = messages.join("\n");
        if out.success && passed > 0 && failed == 0 {
            (TestResult::Passed, None, text)
        } else if failed > 0 && messages.iter().any(|s| s.starts_with("AssertionError:")) {
            (TestResult::Failed, None, text)
        } else if failed > 0 {
            (
                TestResult::CompileError,
                Some(RunDetail::FailedWithoutAssertion),
                text,
            )
        } else if suites
            .iter()
            .filter_map(|suite| suite["message"].as_str())
            .any(|m| m.contains("Cannot find package"))
        {
            (
                TestResult::CompileError,
                Some(RunDetail::MissingDependencies),
                text,
            )
        } else if out.success
            || count("numTotalTests") == 0 && suites.iter().all(|s| s["status"] != "failed")
        {
            (TestResult::CompileError, Some(RunDetail::NoTestRan), text)
        } else {
            (TestResult::CompileError, None, text)
        }
    }
    /// What a Vitest run that printed no JSON report says.
    fn vitest_unreadable(out: &Output) -> RunDetail {
        let stderr = String::from_utf8_lossy(&out.stderr);
        if stderr.contains("ENOTCACHED") || stderr.contains("could not determine executable to run")
        {
            RunDetail::MissingDependencies
        } else if Self::resource_death(out, &stderr) {
            RunDetail::OverLimits
        } else {
            RunDetail::UnreadableOutput
        }
    }
}
fn sh_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}
#[async_trait::async_trait]
impl TestRunner for SandboxedTests {
    async fn run(&self, job: TestJob) -> Result<TestRun> {
        let dir = tempfile::tempdir_in(&self.scratch)?;
        let workspace = dir.path().join("clone");
        repository::snapshot(&job.repository, &job.commit, &workspace)?;
        let target = workspace.join(job.target.path());
        std::fs::create_dir_all(target.parent().ok_or(Error::Operation("test parent"))?)?;
        std::fs::write(&target, &job.raw)?;
        let mut suites = std::collections::BTreeMap::new();
        let (repro, detail, failure_site) = match job.target.kind() {
            TestKind::RustIntegration | TestKind::RustUnit => {
                let parts: Vec<_> = job.target.path().split('/').collect();
                let manifest = workspace.join("crates").join(parts[1]).join("Cargo.toml");
                let root = manifest.parent().ok_or(Error::Operation("test crate"))?;
                repository::register_repro(&workspace, job.target.path())?;
                let test_name =
                    if job.target.kind() == TestKind::RustIntegration && parts.len() == 4 {
                        target
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .ok_or(Error::Operation("repro name"))?
                    } else {
                        "ophio_pipeline_repro"
                    };
                let select = if job.target.kind() == TestKind::RustIntegration {
                    "--test"
                } else {
                    "--lib"
                };
                let cargo = format!(
                    "/usr/bin/cargo test --offline --manifest-path {} {select} {}",
                    sh_quote(&manifest.to_string_lossy()),
                    sh_quote(test_name),
                );
                // Build first, say how that went, then run: a run that dies
                // is never read as a build that failed.
                let marker = format!("ophio-built-{}", protocol::ReportId::new());
                let script = format!(
                    "{cargo} --no-run; s=$?; printf '\\n{marker}:%s\\n' \"$s\"; [ \"$s\" -eq 0 ] || exit \"$s\"; exec {cargo} -- --nocapture"
                );
                let out = self
                    .command(
                        &workspace,
                        "/bin/sh",
                        vec!["-c".into(), script],
                        job.deadline_ms,
                    )
                    .await?;
                let (result, detail) = Self::rust(&out, &marker);
                let site = (result == TestResult::Failed
                    || detail == Some(RunDetail::FailedWithoutAssertion))
                .then(|| {
                    let mut text = out.stderr.clone();
                    text.push(b'\n');
                    text.extend_from_slice(&out.stdout);
                    failure_site(
                        &String::from_utf8_lossy(&text),
                        "crates/",
                        &workspace,
                        job.target.path(),
                    )
                })
                .flatten();
                if job.target.kind() == TestKind::RustIntegration {
                    std::fs::remove_file(&target)?;
                    if parts.len() > 4 {
                        std::fs::remove_file(root.join("tests/ophio_pipeline_repro.rs"))?;
                    }
                }
                (result, detail, site)
            }
            TestKind::Vitest => {
                // Offline caches must contain the complete prepared dependency tree.
                let out = self
                    .command(
                        &workspace,
                        "/usr/bin/npm",
                        vec![
                            "--prefix".into(),
                            workspace.join("client").to_string_lossy().into_owned(),
                            "exec".into(),
                            "--offline".into(),
                            "--".into(),
                            "vitest".into(),
                            "run".into(),
                            target.to_string_lossy().into_owned(),
                            "--root".into(),
                            workspace.join("client").to_string_lossy().into_owned(),
                            "--reporter=json".into(),
                        ],
                        job.deadline_ms,
                    )
                    .await?;
                match serde_json::from_slice::<serde_json::Value>(&out.stdout) {
                    Ok(json) => {
                        let (result, detail, messages) = Self::vitest(&out, &json);
                        let site = (result == TestResult::Failed
                            || detail == Some(RunDetail::FailedWithoutAssertion))
                        .then(|| {
                            failure_site(&messages, "client/src/", &workspace, job.target.path())
                        })
                        .flatten();
                        (result, detail, site)
                    }
                    // The test can write to stdout too; no report is its doing.
                    Err(_) => (
                        TestResult::CompileError,
                        Some(Self::vitest_unreadable(&out)),
                        None,
                    ),
                }
            }
        };
        if job.repro_only {
            return Ok(TestRun {
                commit: job.commit,
                test_hash: hash(&job.raw),
                repro,
                suites,
                detail,
                failure_site,
            });
        }
        if workspace.join("Cargo.toml").is_file() {
            let script = if workspace.join("crates/computer-mcp/Cargo.toml").is_file() {
                "cargo build --offline -p computer-mcp && cargo test --offline --workspace -- --skip ophio_pipeline_repro"
            } else {
                "cargo test --offline --workspace -- --skip ophio_pipeline_repro"
            };
            let all = self
                .command(
                    &workspace,
                    "/bin/sh",
                    vec!["-c".into(), script.into()],
                    job.deadline_ms,
                )
                .await?;
            suites.insert(
                "rust_workspace".into(),
                if all.success {
                    TestResult::Passed
                } else if String::from_utf8_lossy(&all.stderr).contains("could not compile") {
                    TestResult::CompileError
                } else {
                    TestResult::Failed
                },
            );
        }
        if workspace.join("client/package.json").is_file() {
            let mut args = vec![
                "--prefix".into(),
                workspace.join("client").to_string_lossy().into_owned(),
                "exec".into(),
                "--offline".into(),
                "--".into(),
                "vitest".into(),
                "run".into(),
                "--root".into(),
                workspace.join("client").to_string_lossy().into_owned(),
            ];
            if job.target.kind() == TestKind::Vitest {
                args.extend([
                    "--exclude".into(),
                    job.target
                        .path()
                        .strip_prefix("client/")
                        .ok_or(Error::Operation("client test path"))?
                        .into(),
                ]);
            }
            let all = self
                .command(&workspace, "/usr/bin/npm", args, job.deadline_ms)
                .await?;
            suites.insert(
                "client".into(),
                if all.success {
                    TestResult::Passed
                } else {
                    TestResult::Failed
                },
            );
        }
        Ok(TestRun {
            commit: job.commit,
            test_hash: hash(&job.raw),
            repro,
            suites,
            detail,
            failure_site,
        })
    }
}

/// The first `path:line` in `output` under `prefix` that names a regular
/// file of the checked-out `workspace` other than the test itself or the
/// runner's own registration, such as a panic location or a stack frame.
/// The output is the test's to write, so this only ever names a real file.
pub fn failure_site(output: &str, prefix: &str, workspace: &Path, test: &str) -> Option<String> {
    const MAX_SITES: usize = 64;
    let path_byte = |b: u8| b.is_ascii_alphanumeric() || b"/_-.".contains(&b);
    let bytes = output.as_bytes();
    output
        .match_indices(prefix)
        .filter_map(|(at, _)| {
            let end = at + bytes[at..].iter().take_while(|b| path_byte(**b)).count();
            let path = &output[at..end];
            let digits = bytes[end..]
                .strip_prefix(b":")?
                .iter()
                .take_while(|b| b.is_ascii_digit())
                .count();
            let line = &output[end + 1..end + 1 + digits];
            (1..=9).contains(&digits).then_some((path, line))
        })
        .take(MAX_SITES)
        .find(|(path, _)| {
            let name = path.rsplit('/').next().unwrap_or(path);
            path.len() <= 240
                && *path != test
                && name != "ophio_pipeline_repro.rs"
                && [".rs", ".ts", ".tsx", ".js", ".jsx", ".mjs"]
                    .iter()
                    .any(|e| path.ends_with(e))
                && repository::safe_relative(path)
                && !path.split('/').any(|s| s.starts_with('.'))
                && std::fs::symlink_metadata(workspace.join(path)).is_ok_and(|m| m.is_file())
        })
        .map(|(path, line)| format!("{path}:{line}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    const BUILT: &str = "ophio-built-test";
    fn output(code: i32, stdout: &str, stderr: &str) -> Output {
        Output {
            success: code == 0,
            code: Some(code),
            stdout: stdout.as_bytes().to_vec(),
            stderr: stderr.as_bytes().to_vec(),
        }
    }
    /// A run that built, then printed `run` and `stderr` and exited `code`.
    fn ran(code: i32, run: &str, stderr: &str) -> (TestResult, Option<RunDetail>) {
        SandboxedTests::rust(&output(code, &format!("\n{BUILT}:0\n{run}"), stderr), BUILT)
    }
    const ASSERTION: &str = "thread 'lost' panicked at crates/x/tests/lost.rs:3:5:\nassertion `left == right` failed\n  left: 1\n right: 2\n";
    #[test]
    fn rust_evidence_needs_libtest_shapes_and_a_build_before_the_run() {
        assert_eq!(
            ran(0, "test result: ok. 10 passed; 0 failed;", ""),
            (TestResult::Passed, None)
        );
        assert_eq!(
            ran(0, "test result: ok. 0 passed; 0 failed;", ""),
            (TestResult::CompileError, Some(RunDetail::NoTestRan))
        );
        assert_eq!(
            ran(101, "test result: FAILED. 0 passed; 1 failed;", ASSERTION),
            (TestResult::Failed, None)
        );
        assert_eq!(
            ran(
                101,
                "test result: FAILED. 0 passed; 1 failed;",
                "thread 'lost' panicked at crates/x/tests/lost.rs:3:5:\nassertion failed: ready\n"
            )
            .0,
            TestResult::Failed
        );
        // Printing the word, or a panic message that merely mentions it,
        // isn't libtest's shape for an assertion.
        for stderr in [
            "assertion failed somewhere\n",
            "thread 'lost' panicked at crates/x/src/lib.rs:9:1:\ncalled `Option::unwrap()` on a `None` value: no assertion here\n",
        ] {
            assert_eq!(
                ran(
                    101,
                    "test result: FAILED. 0 passed; 1 failed; assertion",
                    stderr
                ),
                (
                    TestResult::CompileError,
                    Some(RunDetail::FailedWithoutAssertion)
                ),
                "{stderr}"
            );
        }
        // Built, then died before a result: a crash, or the runner's limits.
        assert_eq!(
            ran(
                101,
                "running 1 test\n",
                "thread 'lost' has overflowed its stack\nerror: test failed (signal: 6, SIGABRT: process abort signal)"
            ),
            (TestResult::CompileError, Some(RunDetail::Crashed))
        );
        assert_eq!(
            ran(0, "", ""),
            (TestResult::CompileError, Some(RunDetail::Crashed))
        );
        assert_eq!(
            ran(
                101,
                "running 1 test\n",
                "error: test failed (signal: 9, SIGKILL: kill)"
            ),
            (TestResult::CompileError, Some(RunDetail::OverLimits))
        );
        assert_eq!(
            ran(152, "running 1 test\n", ""),
            (TestResult::CompileError, Some(RunDetail::OverLimits))
        );
        // A build that failed never reaches the marker, and only the first
        // marker counts, so the test can't claim a build of its own.
        assert_eq!(
            SandboxedTests::rust(&output(101, "", "error: could not compile `x`"), BUILT),
            (TestResult::CompileError, None)
        );
        assert_eq!(
            SandboxedTests::rust(
                &output(
                    101,
                    &format!("\n{BUILT}:101\n{BUILT}:0\ntest result: ok. 1 passed;"),
                    "error: could not compile `x`"
                ),
                BUILT
            ),
            (TestResult::CompileError, None)
        );
        assert_eq!(
            SandboxedTests::rust(
                &output(
                    101,
                    &format!("\n{BUILT}:101\n"),
                    "error: failed to download `x v1.0.0`\nCaused by:\n  attempting to make an HTTP request, but --offline was specified"
                ),
                BUILT
            ),
            (
                TestResult::CompileError,
                Some(RunDetail::MissingDependencies)
            )
        );
        assert_eq!(
            SandboxedTests::rust(&output(137, "", "   Compiling x v0.1.0"), BUILT),
            (TestResult::CompileError, Some(RunDetail::OverLimits))
        );
    }
    #[test]
    fn vitest_reports_tell_assertions_from_other_failures() {
        let out = |code| output(code, "", "");
        let failing = |message: &str| serde_json::json!({"numPassedTests":0,"numFailedTests":1,"numTotalTests":1,"testResults":[{"status":"failed","assertionResults":[{"status":"failed","failureMessages":[message]}]}]});
        assert_eq!(
            SandboxedTests::vitest(&out(1), &failing("AssertionError: expected 1 to be 2\n    at /w/clone/client/src/state/x.test.ts:3:9")).0,
            TestResult::Failed
        );
        // Mentioning the name isn't Vitest's assertion error.
        assert_eq!(
            SandboxedTests::vitest(&out(1), &failing("Error: not an AssertionError: really")).1,
            Some(RunDetail::FailedWithoutAssertion)
        );
        assert_eq!(
            SandboxedTests::vitest(&out(1), &failing("TypeError: x is undefined")).1,
            Some(RunDetail::FailedWithoutAssertion)
        );
        let missing = serde_json::json!({"numPassedTests":0,"numFailedTests":0,"numTotalTests":0,"testResults":[{"status":"failed","message":"Error: Cannot find package 'left-pad'","assertionResults":[]}]});
        assert_eq!(
            SandboxedTests::vitest(&out(1), &missing).1,
            Some(RunDetail::MissingDependencies)
        );
        let syntax = serde_json::json!({"numPassedTests":0,"numFailedTests":0,"numTotalTests":0,"testResults":[{"status":"failed","message":"SyntaxError","assertionResults":[]}]});
        assert_eq!(
            SandboxedTests::vitest(&out(1), &syntax),
            (TestResult::CompileError, None, String::new())
        );
        let empty = serde_json::json!({"numPassedTests":0,"numFailedTests":0,"numTotalTests":0,"testResults":[]});
        assert_eq!(
            SandboxedTests::vitest(&out(0), &empty).1,
            Some(RunDetail::NoTestRan)
        );
        // No JSON report at all: missing dependencies, limits, or the test's own output.
        assert_eq!(
            SandboxedTests::vitest_unreadable(&output(1, "", "npm error code ENOTCACHED")),
            RunDetail::MissingDependencies
        );
        assert_eq!(
            SandboxedTests::vitest_unreadable(&output(137, "", "")),
            RunDetail::OverLimits
        );
        assert_eq!(
            SandboxedTests::vitest_unreadable(&output(1, "not json", "")),
            RunDetail::UnreadableOutput
        );
    }
    #[test]
    fn shell_quoting_keeps_arguments_whole() {
        assert_eq!(sh_quote("a b"), "'a b'");
        assert_eq!(sh_quote("it's"), "'it'\\''s'");
    }
    #[test]
    fn a_failure_site_names_only_a_real_file_other_than_the_test() {
        let dir = tempfile::tempdir().unwrap();
        for p in [
            "crates/store/src/lib.rs",
            "crates/store/tests/lost.rs",
            "crates/store/tests/ophio_pipeline_repro.rs",
            "client/src/state/real.ts",
        ] {
            let path = dir.path().join(p);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "").unwrap();
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(
            dir.path().join("crates/store/src/lib.rs"),
            dir.path().join("crates/store/src/link.rs"),
        )
        .unwrap();
        let site = |text: &str, prefix| {
            failure_site(text, prefix, dir.path(), "crates/store/tests/lost.rs")
        };
        let panic = "thread 'lost' panicked at crates/store/tests/lost.rs:3:5:\nassertion failed\nstack backtrace:\n   1: core::panicking::panic\n             at /rustc/abc/library/core/src/panicking.rs:1:1\n   2: store::write\n             at ./crates/store/src/lib.rs:12:9\n";
        assert_eq!(
            site(panic, "crates/").as_deref(),
            Some("crates/store/src/lib.rs:12")
        );
        // The test, the runner's own wrapper, symlinks and missing files are skipped.
        assert_eq!(site("at crates/store/tests/lost.rs:3:5", "crates/"), None);
        assert_eq!(
            site("at crates/store/tests/ophio_pipeline_repro.rs:1", "crates/"),
            None
        );
        assert_eq!(site("at crates/store/src/link.rs:1", "crates/"), None);
        assert_eq!(site("at crates/store/src/missing.rs:1", "crates/"), None);
        assert_eq!(site("at crates/store/src/lib.rs", "crates/"), None);
        assert_eq!(
            site("at crates/../crates/store/src/lib.rs:4", "crates/"),
            Some("crates/store/src/lib.rs:4".into())
        );
        assert_eq!(
            site(
                "at /tmp/x/clone/client/src/state/real.ts:7:2",
                "client/src/"
            )
            .as_deref(),
            Some("client/src/state/real.ts:7")
        );
    }
}
