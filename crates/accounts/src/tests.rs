// SPDX-License-Identifier: Apache-2.0
//! Accounts end to end, with stand-ins for `claude`, `codex` and `gh`, an
//! in-memory vault, a browser that only records, and a token endpoint on
//! this computer. Nothing reaches the owner's keyring, stores or browser.
use super::*;
use base64::Engine as _;
use desktop::Recorder;
use ports::accounts::mask;
use protocol::accounts::ServiceKind;
use std::os::unix::fs::PermissionsExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use vault::MemoryVault;

/// Writes an executable stand-in at `path`. A test spawning a tool at the same
/// moment inherits any write handle open in this process until its exec, and
/// running the stand-in then fails as busy; so a child process writes it. It
/// takes the place of the one before whole, which a running copy keeps.
fn put_script(path: &Path, script: &str) {
    use std::io::Write as _;
    let mut child = std::process::Command::new("/bin/sh")
        .args([
            "-c",
            "cat > \"$1.new\" && chmod 700 \"$1.new\" && mv \"$1.new\" \"$1\"",
            "sh",
        ])
        .arg(path)
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(script.as_bytes())
        .unwrap();
    assert!(child.wait().unwrap().success(), "could not write {path:?}");
}

const GH: &str = r#"#!/bin/sh
PATH=/usr/bin:/bin
state="$GH_CONFIG_DIR/hosts.yml"
case "$1 $2" in
"auth login")
  printf '! First copy your one-time code: ABCD-1234\n'
  printf 'Press Enter to open https://github.com/login/device in your browser... '
  read _ || exit 1
  "$GH_BROWSER" https://github.com/login/device
  while [ ! -e "$TEST_GO" ]; do sleep 0.05; done
  printf 'user: octo\n' > "$state" ;;
"auth status")
  if [ -e "$state" ]; then
    printf '{"hosts":{"github.com":[{"state":"success","active":true,"login":"octo"}]}}\n'
  else
    printf '{"hosts":{}}\n'; exit 1
  fi ;;
"auth token") [ -e "$state" ] && printf 'gho_TestToken0123456789\n' || exit 1 ;;
"auth logout") rm -f "$state" ;;
esac
"#;

const CLAUDE: &str = r#"#!/bin/sh
PATH=/usr/bin:/bin
case "$1 $2" in
"auth login")
  printf 'Opening your browser to sign in...\n'
  "$BROWSER" 'https://claude.ai/oauth/authorize?code=true'
  while [ ! -e "$TEST_GO" ]; do sleep 0.05; done
  touch "$HOME/claude-in" ;;
"auth status")
  if [ -e "$HOME/claude-in" ]; then
    printf '{"loggedIn":true,"email":"me@example.com"}\n'
  else
    printf '{"loggedIn":false}\n'
  fi ;;
"auth logout") rm -f "$HOME/claude-in" ;;
esac
"#;

const CODEX: &str = "#!/bin/sh\necho 'Not logged in' >&2\nexit 1\n";

struct Fixture {
    root: tempfile::TempDir,
    accounts: ComputerAccounts,
    vault: Arc<MemoryVault>,
    opener: Arc<Recorder>,
}

impl Fixture {
    fn new() -> Self {
        Self::with(Duration::from_secs(20), Saved::default())
    }

    fn with(sign_in_limit: Duration, saved: Saved) -> Self {
        Self::with_vault(sign_in_limit, saved, None)
    }

    /// `other` keeps the values in place of the fixture's own vault.
    fn with_vault(sign_in_limit: Duration, saved: Saved, other: Option<Arc<dyn Vault>>) -> Self {
        let root = tempfile::tempdir().unwrap();
        let bin = root.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        for (name, script) in [("gh", GH), ("claude", CLAUDE), ("codex", CODEX)] {
            put_script(&bin.join(name), script);
        }
        let home = root.path().join("home");
        std::fs::create_dir(&home).unwrap();
        let dir = root.path().join("data").join("accounts");
        Store::new(&dir).save(&saved).unwrap();
        let vault = Arc::new(MemoryVault::default());
        let opener = Arc::new(Recorder::default());
        let accounts = ComputerAccounts::new(Config {
            dir,
            vault: other.unwrap_or_else(|| vault.clone()),
            opener: opener.clone(),
            tools: Tools {
                env: vec![
                    ("PATH".into(), bin.into()),
                    ("HOME".into(), home.into()),
                    ("TEST_GO".into(), root.path().join("go").into()),
                ],
            },
            http: http(),
            sign_in_limit,
        });
        Self {
            root,
            accounts,
            vault,
            opener,
        }
    }

    /// Replaces the stand-in `name` with `script`.
    fn script(&self, name: &str, script: &str) {
        put_script(&self.root.path().join("bin").join(name), script);
    }

    /// Lets a stand-in sign-in finish.
    fn go(&self) {
        std::fs::write(self.root.path().join("go"), "").unwrap();
    }

    fn dir(&self) -> &Path {
        &self.accounts.config().dir
    }

    async fn act(&self, action: AccountAction) -> Result<(), AccountsError> {
        self.accounts.act(action, "Test phone").await
    }

    async fn account(&self, id: &str) -> Account {
        self.accounts
            .view()
            .await
            .accounts
            .into_iter()
            .find(|a| a.account_id == id)
            .unwrap_or_else(|| panic!("no account {id}"))
    }

    /// Waits for the view to show what `what` looks for.
    async fn until<T>(&self, what: impl Fn(&AccountsView) -> Option<T>) -> T {
        let mut changed = self.accounts.changed();
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                if let Some(found) = what(&self.accounts.view().await) {
                    return found;
                }
                changed.changed().await.unwrap();
            }
        })
        .await
        .expect("the accounts did not get there in time")
    }

    async fn until_stage(&self, want: impl Fn(&SignInStage) -> bool) -> SignInStage {
        self.until(|v| {
            v.sign_in
                .as_ref()
                .map(|s| s.stage.clone())
                .filter(|stage| want(stage))
        })
        .await
    }

    async fn until_state(&self, id: &str, want: AccountState) {
        self.until(|v| {
            v.accounts
                .iter()
                .any(|a| a.account_id == id && a.state == want)
                .then_some(())
        })
        .await;
    }

    /// Everything Ophio wrote to disk, as one string.
    fn written(&self) -> String {
        fn walk(dir: &Path, into: &mut String) {
            for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, into);
                } else if let Ok(text) = std::fs::read_to_string(&path) {
                    into.push_str(&text);
                }
            }
        }
        let mut all = String::new();
        walk(self.dir(), &mut all);
        all
    }
}

fn key_service(name: &str, variable: &str) -> ServiceProfile {
    ServiceProfile {
        name: name.into(),
        service: ServiceKind::ApiKey {
            variable: variable.into(),
            check_url: None,
            manage_url: Some("https://example.com/keys".into()),
        },
    }
}

fn env_of(given: &RunAccounts, name: &str) -> Option<String> {
    given
        .env
        .0
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.clone())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn github_signs_in_with_its_code_and_tasks_get_its_own_folder() {
    let f = Fixture::new();
    f.act(AccountAction::SignIn {
        account_id: "github".into(),
    })
    .await
    .unwrap();
    // gh asked for Enter, printed its code and opened its page through Ophio.
    f.until_stage(|s| {
        *s == SignInStage::Code {
            code: "ABCD-1234".into(),
            url_host: "github.com".into(),
        }
    })
    .await;
    assert!(matches!(
        f.act(AccountAction::SignIn {
            account_id: "claude".into()
        })
        .await,
        Err(AccountsError::Busy(_))
    ));
    f.go();
    let done = f
        .until_stage(|s| matches!(s, SignInStage::Done { .. }))
        .await;
    assert_eq!(
        done,
        SignInStage::Done {
            identity: Some("octo".into())
        }
    );
    assert_eq!(f.opener.opened(), ["https://github.com/login/device"]);
    let github = f.account("github").await;
    assert_eq!(github.state, AccountState::Active);
    assert_eq!(github.identity.as_deref(), Some("octo"));
    let history = f.accounts.view().await.history;
    assert_eq!(history[0].event, AccountEvent::SignedIn);
    assert_eq!(history[0].by, "Test phone");
    assert_eq!(history[0].detail.as_deref(), Some("octo"));

    // The sign-in lives in the account's own private folder.
    let folder = f.dir().join("config").join("github");
    assert!(folder.join("hosts.yml").is_file());
    let mode = std::fs::metadata(&folder).unwrap().permissions().mode();
    assert_eq!(mode & 0o077, 0, "only the owner can open {folder:?}");
    assert!(!f.dir().join("signin").exists(), "the scratch is emptied");

    // A task gets the folder, and gh's token is masked in what it says.
    let task = TaskId::new();
    let given = f
        .accounts
        .for_run(HarnessKind::ClaudeCode, task)
        .await
        .unwrap();
    assert_eq!(
        env_of(&given, "GH_CONFIG_DIR").as_deref(),
        Some(folder.to_str().unwrap())
    );
    let mut said = "pushed with gho_TestToken0123456789 ok".to_owned();
    mask(&mut said, &given.secrets);
    assert_eq!(said, format!("pushed with {} ok", ports::accounts::MASK));
    assert_eq!(f.account("github").await.used_by, [task]);
    assert_eq!(f.account("claude").await.used_by, [task]);
    f.accounts.run_ended(task);
    assert!(f.account("github").await.used_by.is_empty());
    assert!(f.account("claude").await.used_by.is_empty());

    // `ophio token GitHub` gives the tool's current token.
    assert_eq!(
        f.accounts.token("GitHub").await.unwrap().as_str(),
        "gho_TestToken0123456789"
    );
    assert!(
        !f.written().contains("gho_TestToken"),
        "no token in Ophio's files"
    );

    // Signing out runs gh's own sign-out and removes the folder.
    f.act(AccountAction::SignOut {
        account_id: "github".into(),
    })
    .await
    .unwrap();
    assert!(!folder.exists());
    assert_eq!(f.account("github").await.state, AccountState::SignedOut);
    assert!(matches!(
        f.accounts.token("github").await,
        Err(AccountsError::NotReady(_))
    ));
    let given = f
        .accounts
        .for_run(HarnessKind::ClaudeCode, TaskId::new())
        .await
        .unwrap();
    assert_eq!(env_of(&given, "GH_CONFIG_DIR"), None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn github_without_permission_for_workflows_says_to_sign_in_again() {
    let f = Fixture::new();
    // gh keeps the scopes it was asked for; it was signed in before Ophio
    // asked for workflows.
    f.script(
        "gh",
        &GH.replace(
            r#"  printf 'user: octo\n' > "$state" ;;"#,
            r#"  printf 'user: octo\nscopes: %s\n' "$*" > "$state" ;;"#,
        )
        .replace(
            r#"    printf '{"hosts":{"github.com":[{"state":"success","active":true,"login":"octo"}]}}\n'"#,
            r#"    grep -q -- '--scopes workflow' "$state" && more=', workflow' || more=''
    printf '{"hosts":{"github.com":[{"state":"success","active":true,"login":"octo","scopes":"gist, read:org, repo%s"}]}}\n' "$more""#,
        ),
    );
    let dir = f.dir().join("config").join("github");
    files::private_dir(&dir).unwrap();
    std::fs::write(dir.join("hosts.yml"), "user: octo\nscopes: auth login\n").unwrap();
    f.accounts.check(Some("github")).await;
    let github = f.account("github").await;
    assert_eq!(github.state, AccountState::Active, "still usable");
    assert_eq!(github.note.as_deref(), Some(check::WORKFLOW_NOTE));

    // Signing in again asks GitHub for it.
    f.act(AccountAction::SignIn {
        account_id: "github".into(),
    })
    .await
    .unwrap();
    f.until_stage(|s| matches!(s, SignInStage::Code { .. }))
        .await;
    f.go();
    f.until_stage(|s| matches!(s, SignInStage::Done { .. }))
        .await;
    let github = f.account("github").await;
    assert_eq!(github.state, AccountState::Active);
    assert_eq!(github.note, None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn claude_signs_in_through_a_terminal_and_signs_out_with_its_own_command() {
    let f = Fixture::new();
    f.act(AccountAction::SignIn {
        account_id: "claude".into(),
    })
    .await
    .unwrap();
    f.until_stage(|s| {
        *s == SignInStage::Browser {
            url_host: "claude.ai".into(),
        }
    })
    .await;
    f.go();
    let done = f
        .until_stage(|s| matches!(s, SignInStage::Done { .. }))
        .await;
    assert_eq!(
        done,
        SignInStage::Done {
            identity: Some("me@example.com".into())
        }
    );
    assert_eq!(
        f.opener.opened(),
        ["https://claude.ai/oauth/authorize?code=true"]
    );
    f.act(AccountAction::SignOut {
        account_id: "claude".into(),
    })
    .await
    .unwrap();
    let claude = f.account("claude").await;
    assert_eq!(claude.state, AccountState::SignedOut);
    assert_eq!(claude.identity, None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn forgetting_all_ends_what_it_can_and_keeps_the_agents_logins() {
    let f = Fixture::new();
    for id in ["github", "claude"] {
        f.act(AccountAction::SignIn {
            account_id: id.into(),
        })
        .await
        .unwrap();
        f.go();
        f.until_stage(|s| matches!(s, SignInStage::Done { .. }))
            .await;
        std::fs::remove_file(f.root.path().join("go")).unwrap();
    }
    f.act(AccountAction::SetKey {
        account_id: "openrouter".into(),
        key: Secret::new("sk-or-Value0123456789"),
    })
    .await
    .unwrap();
    let hosts = f.dir().join("config").join("github").join("hosts.yml");
    assert!(hosts.is_file());

    let (open, emptied) = f.accounts.forget_all().await;
    emptied.unwrap();
    assert!(f.vault.is_empty());
    assert!(!hosts.exists(), "gh's own sign-out ran");
    // gh forgets its token without ending it, and a key works until it is
    // deleted at the provider: both are named with their pages.
    let names: Vec<_> = open.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(names, ["GitHub", "OpenRouter"]);
    assert_eq!(open[0].1, "https://github.com/settings/applications");
    // Claude Code's login is the owner's own.
    assert!(f.root.path().join("home").join("claude-in").exists());
    assert_eq!(f.opener.opened().len(), 2, "nothing more is opened");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_sign_in_can_be_cancelled_and_runs_out_of_time() {
    let f = Fixture::with(Duration::from_secs(3), Saved::default());
    assert!(matches!(
        f.act(AccountAction::SignIn {
            account_id: "openrouter".into()
        })
        .await,
        Err(AccountsError::Invalid(_))
    ));
    assert!(matches!(
        f.act(AccountAction::SignIn {
            account_id: "google".into()
        })
        .await,
        Err(AccountsError::Invalid(_))
    ));

    f.act(AccountAction::SignIn {
        account_id: "github".into(),
    })
    .await
    .unwrap();
    f.until_stage(|s| matches!(s, SignInStage::Code { .. }))
        .await;
    f.act(AccountAction::CancelSignIn).await.unwrap();
    f.until_stage(|s| *s == SignInStage::Cancelled).await;
    let history = f.accounts.view().await.history;
    assert_eq!(history[0].event, AccountEvent::SignInFailed);
    assert_eq!(history[0].detail.as_deref(), Some("Cancelled"));
    assert_eq!(f.account("github").await.state, AccountState::SignedOut);

    // The next one may start, and stops at the limit.
    f.act(AccountAction::SignIn {
        account_id: "github".into(),
    })
    .await
    .unwrap();
    f.until_stage(|s| *s == SignInStage::TimedOut).await;
    assert!(!f.dir().join("signin").exists());
}

/// Left behind by a sign-in: it ignores being asked to stop, and once
/// `TEST_GO` exists writes the login at `$1`, as a helper that outlived its
/// sign-in would restore a removed one. Its id is in `$HOME/$2.pid`.
const LINGER: &str = r#"#!/bin/sh
PATH=/usr/bin:/bin
trap '' TERM HUP INT
echo $$ > "$HOME/$2.tmp" && mv "$HOME/$2.tmp" "$HOME/$2.pid"
while [ ! -e "$TEST_GO" ]; do sleep 0.05; done
mkdir -p "$(dirname "$1")"
printf 'restored\n' > "$1"
touch "$HOME/$2.wrote"
"#;

/// Signs in by leaving `linger` behind, and another in a session of its own
/// where `$HOME/escape` exists, then waits.
const LINGERING_LOGIN: &str = r#"#!/bin/sh
PATH=/usr/bin:/bin
bin="$(dirname "$0")"
case "$1 $2" in
"auth login")
  state="${GH_CONFIG_DIR:-$CLAUDE_CONFIG_DIR}/login"
  "$bin/linger" "$state" stayed &
  [ -e "$HOME/escape" ] && setsid "$bin/linger" "$state" escaped &
  printf '! First copy your one-time code: ABCD-1234\n'
  while :; do sleep 0.05; done ;;
"auth status") printf '{"hosts":{}}\n'; exit 1 ;;
esac
"#;

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nothing_a_stopped_sign_in_started_outlives_it() {
    let f = Fixture::new();
    let bin = f.root.path().join("bin");
    let home = f.root.path().join("home");
    put_script(&bin.join("linger"), LINGER);
    // gh's sign-in has pipes, Claude Code's a terminal.
    f.script("gh", LINGERING_LOGIN);
    f.script("claude", LINGERING_LOGIN);
    // A cgroup holds even what leaves the sign-in's session; a process group
    // alone does not, as the run's containment says.
    let tags: &[&str] = if matches!(
        containment::Container::new().unwrap().containment(),
        ports::Containment::Complete
    ) {
        std::fs::write(home.join("escape"), "").unwrap();
        &["stayed", "escaped"]
    } else {
        &["stayed"]
    };
    let helpers = || async {
        let mut pids = Vec::new();
        for tag in tags {
            let file = home.join(format!("{tag}.pid"));
            let pid = loop {
                if let Ok(pid) = std::fs::read_to_string(&file) {
                    break pid.trim().parse::<u32>().unwrap();
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            };
            std::fs::remove_file(&file).unwrap();
            pids.push(pid);
        }
        pids
    };
    // Running and not yet ended: a zombie has ended.
    let running = |pid: u32| {
        std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
            !stat
                .rsplit(')')
                .next()
                .unwrap_or("")
                .trim_start()
                .starts_with('Z')
        })
    };

    // A cancelled sign-in says so only once nothing it started is left.
    f.act(AccountAction::SignIn {
        account_id: "github".into(),
    })
    .await
    .unwrap();
    f.until_stage(|s| matches!(s, SignInStage::Code { .. }))
        .await;
    let pids = helpers().await;
    assert!(pids.iter().all(|&pid| running(pid)));
    f.act(AccountAction::CancelSignIn).await.unwrap();
    f.until_stage(|s| *s == SignInStage::Cancelled).await;
    assert!(pids.iter().all(|&pid| !running(pid)), "{pids:?} still run");

    // Signing out during a sign-in on a terminal ends all it started first.
    f.act(AccountAction::SignIn {
        account_id: "claude".into(),
    })
    .await
    .unwrap();
    f.until_stage(|s| matches!(s, SignInStage::Code { .. }))
        .await;
    let pids = helpers().await;
    assert!(pids.iter().all(|&pid| running(pid)));
    f.act(AccountAction::SignOut {
        account_id: "claude".into(),
    })
    .await
    .unwrap();
    assert!(pids.iter().all(|&pid| !running(pid)), "{pids:?} still run");

    // Nothing is left to write a login back.
    f.go();
    tokio::time::sleep(Duration::from_millis(500)).await;
    for tag in tags {
        assert!(!home.join(format!("{tag}.wrote")).exists());
    }
    assert!(!f.dir().join("config").join("claude").exists());
    assert!(!f.dir().join("config").join("github").join("login").exists());
}

/// A sign-in that prints on both streams without end, the helper printing
/// on the second having its id in `$HOME/err.pid`.
const FLOODING_LOGIN: &str = r#"#!/bin/sh
PATH=/usr/bin:/bin
case "$1 $2" in
"auth login")
  yes err >&2 &
  echo $! > "$HOME/err.pid"
  exec yes out ;;
*) exit 1 ;;
esac
"#;

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_sign_in_that_prints_without_end_is_ended_at_the_output_limit() {
    let f = Fixture::new();
    let home = f.root.path().join("home");
    // gh's sign-in has pipes, Claude Code's a terminal.
    for (tool, account) in [("gh", "github"), ("claude", "claude")] {
        f.script(tool, FLOODING_LOGIN);
        let began = std::time::Instant::now();
        f.act(AccountAction::SignIn {
            account_id: account.into(),
        })
        .await
        .unwrap();
        let SignInStage::Failed { reason } = f
            .until_stage(|s| matches!(s, SignInStage::Failed { .. }))
            .await
        else {
            unreachable!()
        };
        assert_eq!(reason, format!("{tool} printed more than Ophio reads"));
        assert!(
            began.elapsed() < Duration::from_secs(10),
            "{:?}",
            began.elapsed()
        );
        // Said only once nothing it started is left.
        let err = std::fs::read_to_string(home.join("err.pid")).unwrap();
        let running =
            std::fs::read_to_string(format!("/proc/{}/stat", err.trim())).is_ok_and(|stat| {
                !(stat.rsplit(')').next().unwrap_or("").trim_start()).starts_with('Z')
            });
        assert!(!running, "{tool}'s helper still runs");
        std::fs::remove_file(home.join("err.pid")).unwrap();
    }
}

/// Asks for Enter on and on in a raw terminal, never reading it, then
/// waits with its id in `$HOME/deaf.pid`.
const DEAF_LOGIN: &str = r#"#!/bin/sh
PATH=/usr/bin:/bin
case "$1 $2" in
"auth login")
  stty raw -echo
  yes 'Press Enter' | head -n 80000 | tr -d '\n'
  echo $$ > "$HOME/deaf.tmp" && mv "$HOME/deaf.tmp" "$HOME/deaf.pid"
  while :; do sleep 0.05; done ;;
*) exit 1 ;;
esac
"#;

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_sign_in_that_asks_for_enter_without_reading_still_stops() {
    let f = Fixture::with(Duration::from_secs(3), Saved::default());
    let home = f.root.path().join("home");
    f.script("claude", DEAF_LOGIN);
    let asked = || async {
        let file = home.join("deaf.pid");
        let pid = loop {
            if let Ok(pid) = std::fs::read_to_string(&file) {
                break pid.trim().to_owned();
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        std::fs::remove_file(&file).unwrap();
        pid
    };
    let running = |pid: &str| {
        std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
            !(stat.rsplit(')').next().unwrap_or("").trim_start()).starts_with('Z')
        })
    };
    let sign_in = || {
        f.act(AccountAction::SignIn {
            account_id: "claude".into(),
        })
    };

    sign_in().await.unwrap();
    let pid = asked().await;
    let began = std::time::Instant::now();
    f.act(AccountAction::CancelSignIn).await.unwrap();
    f.until_stage(|s| *s == SignInStage::Cancelled).await;
    assert!(
        began.elapsed() < Duration::from_secs(5),
        "{:?}",
        began.elapsed()
    );
    assert!(!running(&pid), "the cancelled sign-in still runs");

    sign_in().await.unwrap();
    let pid = asked().await;
    f.until_stage(|s| *s == SignInStage::TimedOut).await;
    assert!(!running(&pid), "the timed-out sign-in still runs");
}

/// Exits at once, leaving a helper in a session of its own that keeps the
/// output open. The helper's id is in `$HOME/held.pid`.
const HOLDS_OUTPUT: &str = r#"#!/bin/sh
PATH=/usr/bin:/bin
setsid sh -c 'echo $$ > "$HOME/held.tmp" && mv "$HOME/held.tmp" "$HOME/held.pid" && exec sleep 30' &
# The parent may exit only once its helper is ready; containment can end
# everything it left immediately after that exit.
while [ ! -s "$HOME/held.pid" ]; do sleep 0.005; done
echo done
"#;

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_tool_keeps_to_its_limit_when_a_helper_holds_its_output() {
    let root = tempfile::tempdir().unwrap();
    let bin = root.path().join("bin");
    let home = root.path().join("home");
    std::fs::create_dir(&bin).unwrap();
    std::fs::create_dir(&home).unwrap();
    put_script(&bin.join("holder"), HOLDS_OUTPUT);
    let tools = Tools {
        env: vec![
            ("PATH".into(), bin.into()),
            ("HOME".into(), home.clone().into()),
        ],
    };

    let began = std::time::Instant::now();
    let ran = tokio::time::timeout(
        Duration::from_secs(10),
        tools.run(&["holder".into()], &[], Duration::from_secs(1)),
    )
    .await
    .expect("the run kept to no limit");
    assert!(
        began.elapsed() < Duration::from_secs(4),
        "{:?}",
        began.elapsed()
    );
    let held = std::fs::read_to_string(home.join("held.pid")).unwrap();
    let held = held.trim();
    // Running and not yet ended: a zombie has ended.
    let running = std::fs::read_to_string(format!("/proc/{held}/stat"))
        .is_ok_and(|stat| !(stat.rsplit(')').next().unwrap_or("").trim_start()).starts_with('Z'));
    // A cgroup ends even what left the tool's session; a process group
    // alone does not, and its output is then given up at the limit.
    if matches!(
        containment::Container::new().unwrap().containment(),
        ports::Containment::Complete
    ) {
        assert_eq!(ran.unwrap().stdout, "done\n");
        assert!(!running);
    } else {
        assert_eq!(ran.unwrap_err(), "holder did not answer in time");
        let _ = std::process::Command::new("kill").arg(held).status();
    }
}

/// Prints without end on both streams. The helper writing to stderr has its
/// id in `$HOME/err.pid`.
const FLOODS: &str = r#"#!/bin/sh
PATH=/usr/bin:/bin
yes err >&2 &
echo $! > "$HOME/err.pid"
exec yes out
"#;

/// Prints past the limit and exits.
const PRINTS_TOO_MUCH: &str = r#"#!/bin/sh
PATH=/usr/bin:/bin
head -c 3000000 /dev/zero | tr '\0' x
"#;

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_tool_that_prints_without_end_is_ended_at_the_output_limit() {
    let root = tempfile::tempdir().unwrap();
    let bin = root.path().join("bin");
    let home = root.path().join("home");
    std::fs::create_dir(&bin).unwrap();
    std::fs::create_dir(&home).unwrap();
    for (name, script) in [("flood", FLOODS), ("chatty", PRINTS_TOO_MUCH)] {
        put_script(&bin.join(name), script);
    }
    let tools = Tools {
        env: vec![
            ("PATH".into(), bin.into()),
            ("HOME".into(), home.clone().into()),
        ],
    };

    let began = std::time::Instant::now();
    let ran = tools
        .run(&["flood".into()], &[], Duration::from_secs(20))
        .await;
    assert_eq!(ran.unwrap_err(), "flood printed more than Ophio reads");
    assert!(
        began.elapsed() < Duration::from_secs(5),
        "{:?}",
        began.elapsed()
    );
    let err = std::fs::read_to_string(home.join("err.pid")).unwrap();
    let running = std::fs::read_to_string(format!("/proc/{}/stat", err.trim()))
        .is_ok_and(|stat| !(stat.rsplit(')').next().unwrap_or("").trim_start()).starts_with('Z'));
    assert!(!running);

    // Exiting first doesn't make the rest of its output welcome.
    let ran = tools
        .run(&["chatty".into()], &[], Duration::from_secs(20))
        .await;
    assert_eq!(ran.unwrap_err(), "chatty printed more than Ophio reads");
    const { assert!(crate::tool::OUTPUT_LIMIT < 3_000_000) };
}

#[tokio::test]
async fn keys_stay_in_the_vault_and_are_masked_in_what_tasks_say() {
    let f = Fixture::new();
    f.act(AccountAction::AddService {
        service: key_service("Groq", "GROQ_API_KEY"),
    })
    .await
    .unwrap();
    let groq = f.account("groq").await;
    assert_eq!(groq.needs, Some(AccountNeed::Key));
    assert!(!groq.browser_sign_in);
    assert_eq!(groq.variables, ["GROQ_API_KEY"]);

    // Too short to be hidden reliably in what tasks say, so not taken.
    for bad in ["", "two words", "tab\there", "gsk_123"] {
        assert!(matches!(
            f.act(AccountAction::SetKey {
                account_id: "groq".into(),
                key: Secret::new(bad),
            })
            .await,
            Err(AccountsError::Invalid(_))
        ));
    }
    assert!(matches!(
        f.act(AccountAction::SetKey {
            account_id: "github".into(),
            key: Secret::new("gsk_Value0123456789"),
        })
        .await,
        Err(AccountsError::Invalid(_))
    ));
    assert!(f.vault.is_empty());

    f.act(AccountAction::SetKey {
        account_id: "groq".into(),
        key: Secret::new("  gsk_Value0123456789\n"),
    })
    .await
    .unwrap();
    // A service without a check address counts a key as signed in.
    f.until_state("groq", AccountState::Active).await;
    assert_eq!(
        f.vault
            .get("groq", Item::Key)
            .await
            .unwrap()
            .unwrap()
            .as_str(),
        "gsk_Value0123456789"
    );
    let view = f.accounts.view().await;
    assert_eq!(view.history[0].event, AccountEvent::KeySet);
    assert_eq!(view.store, "Memory");
    assert!(!serde_json::to_string(&view).unwrap().contains("gsk_Value"));
    assert!(!f.written().contains("gsk_Value"));

    let given = f
        .accounts
        .for_run(HarnessKind::Codex, TaskId::new())
        .await
        .unwrap();
    assert_eq!(
        env_of(&given, "GROQ_API_KEY").as_deref(),
        Some("gsk_Value0123456789")
    );
    assert!(!format!("{given:?}").contains("gsk_Value"));
    let mut said = "key=gsk_Value0123456789".to_owned();
    mask(&mut said, &given.secrets);
    assert!(!said.contains("gsk_Value"));
    assert_eq!(
        f.accounts.token("Groq").await.unwrap().as_str(),
        "gsk_Value0123456789"
    );

    // A key replaced while tasks run stays hidden in them, and so does the new one.
    f.act(AccountAction::SetKey {
        account_id: "groq".into(),
        key: Secret::new("gsk_Other0123456789"),
    })
    .await
    .unwrap();
    let seen = f.accounts.secrets();
    for key in ["gsk_Value0123456789", "gsk_Other0123456789"] {
        assert!(seen.iter().any(|s| s == key), "{key}");
    }
}

#[tokio::test]
async fn paused_accounts_are_given_to_no_task() {
    let f = Fixture::new();
    f.act(AccountAction::AddService {
        service: key_service("Groq", "GROQ_API_KEY"),
    })
    .await
    .unwrap();
    f.act(AccountAction::SetKey {
        account_id: "groq".into(),
        key: Secret::new("gsk_Value0123456789"),
    })
    .await
    .unwrap();
    f.until_state("groq", AccountState::Active).await;

    f.act(AccountAction::Pause {
        account_id: "groq".into(),
    })
    .await
    .unwrap();
    assert_eq!(f.account("groq").await.state, AccountState::Paused);
    let given = f
        .accounts
        .for_run(HarnessKind::Codex, TaskId::new())
        .await
        .unwrap();
    assert_eq!(env_of(&given, "GROQ_API_KEY"), None);
    assert!(given.secrets.is_empty());
    assert!(matches!(
        f.accounts.token("groq").await,
        Err(AccountsError::NotReady(_))
    ));
    f.act(AccountAction::Resume {
        account_id: "groq".into(),
    })
    .await
    .unwrap();
    let given = f
        .accounts
        .for_run(HarnessKind::Codex, TaskId::new())
        .await
        .unwrap();
    assert!(env_of(&given, "GROQ_API_KEY").is_some());

    // A paused agent account stops tasks on that agent only.
    f.act(AccountAction::Pause {
        account_id: "claude".into(),
    })
    .await
    .unwrap();
    let refused = f
        .accounts
        .for_run(HarnessKind::ClaudeCode, TaskId::new())
        .await
        .unwrap_err();
    assert!(matches!(refused, AccountsError::NotReady(_)));
    assert!(refused.to_string().contains("Claude Code is paused"));
    assert!(
        f.accounts
            .for_run(HarnessKind::Codex, TaskId::new())
            .await
            .is_ok()
    );

    f.act(AccountAction::PauseAll).await.unwrap();
    let view = f.accounts.view().await;
    assert_eq!(view.history[0].event, AccountEvent::PausedAll);
    for harness in [HarnessKind::ClaudeCode, HarnessKind::Codex] {
        assert!(f.accounts.for_run(harness, TaskId::new()).await.is_err());
    }
    assert!(matches!(
        f.act(AccountAction::Pause {
            account_id: "nothing".into()
        })
        .await,
        Err(AccountsError::NotFound(_))
    ));
}

#[tokio::test]
async fn deleting_an_added_service_forgets_it_and_its_key() {
    let f = Fixture::new();
    f.act(AccountAction::AddService {
        service: key_service("Groq", "GROQ_API_KEY"),
    })
    .await
    .unwrap();
    f.act(AccountAction::SetKey {
        account_id: "groq".into(),
        key: Secret::new("gsk_Value0123456789"),
    })
    .await
    .unwrap();
    f.act(AccountAction::Delete {
        account_id: "groq".into(),
    })
    .await
    .unwrap();
    assert!(f.vault.is_empty());
    let view = f.accounts.view().await;
    assert!(view.accounts.iter().all(|a| a.account_id != "groq"));
    assert_eq!(view.history[0].event, AccountEvent::Deleted);
    // The key may still work at the provider, so its page is opened.
    assert_eq!(f.opener.opened(), ["https://example.com/keys"]);

    // A built-in account stays, signed out.
    f.act(AccountAction::Delete {
        account_id: "openrouter".into(),
    })
    .await
    .unwrap();
    assert_eq!(f.account("openrouter").await.state, AccountState::SignedOut);
}

#[tokio::test]
async fn an_added_service_cannot_shadow_another_or_what_the_computer_needs() {
    let f = Fixture::new();
    for (service, why) in [
        (key_service("Git Hub", "GIT_HUB_KEY"), "already a service"),
        (key_service("Thing", "PATH"), "needed by the computer"),
        (
            key_service("Gemini", "OPENROUTER_API_KEY"),
            "OpenRouter already gives",
        ),
    ] {
        let refused = f
            .act(AccountAction::AddService { service })
            .await
            .unwrap_err();
        assert!(refused.to_string().contains(why), "{refused}");
    }
    assert!(f.accounts.view().await.history.is_empty());
}

#[tokio::test]
async fn what_the_owner_did_survives_a_restart() {
    let f = Fixture::new();
    f.act(AccountAction::AddService {
        service: key_service("Groq", "GROQ_API_KEY"),
    })
    .await
    .unwrap();
    f.act(AccountAction::Pause {
        account_id: "groq".into(),
    })
    .await
    .unwrap();
    f.act(AccountAction::PasswordManagerDone).await.unwrap();
    std::fs::create_dir_all(f.dir().join("signin")).unwrap();

    let again = ComputerAccounts::new(Config {
        dir: f.dir().to_path_buf(),
        vault: f.vault.clone(),
        opener: f.opener.clone(),
        tools: f.accounts.config().tools.clone(),
        http: http(),
        sign_in_limit: Duration::from_secs(5),
    });
    let view = again.view().await;
    let groq = view
        .accounts
        .iter()
        .find(|a| a.account_id == "groq")
        .unwrap();
    assert_eq!(groq.state, AccountState::Paused);
    assert!(view.password_manager_done);
    assert_eq!(view.history.len(), 2);
    assert!(!f.dir().join("signin").exists(), "leftovers are cleared");
}

#[tokio::test]
async fn checks_read_each_tools_own_answer() {
    let f = Fixture::new();
    std::fs::write(
        f.accounts.config().tools.home().unwrap().join("claude-in"),
        "",
    )
    .unwrap();
    f.accounts.check(None).await;
    let claude = f.account("claude").await;
    assert_eq!(claude.state, AccountState::Active);
    assert_eq!(claude.identity.as_deref(), Some("me@example.com"));
    assert!(claude.checked_at.is_some());
    assert_eq!(f.account("codex").await.state, AccountState::SignedOut);
    assert_eq!(f.account("github").await.state, AccountState::SignedOut);
    assert_eq!(f.account("openrouter").await.state, AccountState::SignedOut);

    // An agent that was signed in and then disappears needs attention.
    std::fs::remove_file(f.accounts.config().tools.find("claude")).unwrap();
    f.accounts.check(Some("claude")).await;
    assert!(matches!(
        f.account("claude").await.state,
        AccountState::NeedsAttention { .. }
    ));
}

// ---- OAuth against a token endpoint on this computer -----------------------

/// The form one request to the token endpoint sent.
async fn read_form(stream: &mut tokio::net::TcpStream) -> String {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let n = stream.read(&mut chunk).await.unwrap();
        buf.extend_from_slice(&chunk[..n]);
        let text = String::from_utf8_lossy(&buf);
        if let Some(head) = text.find("\r\n\r\n") {
            let length: usize = text[..head]
                .lines()
                .find_map(|l| {
                    l.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|v| v.trim().parse().unwrap())
                })
                .unwrap_or(0);
            if buf.len() >= head + 4 + length || n == 0 {
                break;
            }
        }
    }
    let text = String::from_utf8_lossy(&buf).into_owned();
    text.split("\r\n\r\n").nth(1).unwrap_or("").to_owned()
}

async fn answer(mut stream: tokio::net::TcpStream, body: serde_json::Value) {
    let body = body.to_string();
    let reply = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(reply.as_bytes()).await.unwrap();
    let _ = stream.shutdown().await;
}

/// The next connection to a stand-in server. Øphio, if it runs on this
/// computer, asks what a server started in a project folder serves, to offer
/// it as a preview; those asks are turned away rather than answered as the
/// test's.
async fn accept(listener: &tokio::net::TcpListener) -> tokio::net::TcpStream {
    loop {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut head = [0u8; 2048];
        let mut n = 0;
        for _ in 0..100 {
            n = stream.peek(&mut head).await.unwrap_or(0);
            if n == 0 || n == head.len() || head[..n].windows(4).any(|w| w == b"\r\n\r\n") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        let text = String::from_utf8_lossy(&head[..n]).to_ascii_lowercase();
        let preview = text.starts_with("head ")
            || text.contains("\r\naccept: text/html")
            || text.contains("\r\nrange: bytes=0-0");
        if !preview {
            return stream;
        }
        let _ = stream
            .write_all(b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
            .await;
    }
}

/// Answers the token endpoint twice: the code, then a refresh.
async fn token_endpoint() -> (String, tokio::task::JoinHandle<Vec<String>>) {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let url = format!("http://{}/token", listener.local_addr().unwrap());
    let served = tokio::spawn(async move {
        let mut forms = Vec::new();
        // The code, then the one refresh the check asks for.
        for _ in 0..2 {
            let mut stream = accept(&listener).await;
            let form = read_form(&mut stream).await;
            let id_token = format!(
                "h.{}.s",
                base64::engine::general_purpose::URL_SAFE_NO_PAD
                    .encode(br#"{"email":"owner@example.com"}"#)
            );
            let body = if form.contains("grant_type=authorization_code") {
                serde_json::json!({
                    "access_token": "ya29.FirstAccess0123",
                    "expires_in": 3600,
                    "refresh_token": "1//Refresh0123456789",
                    "id_token": id_token,
                })
            } else if form.contains("refresh_token=1%2F%2FRefresh0123456789") {
                serde_json::json!({"access_token": "ya29.Refreshed0123", "expires_in": 3600})
            } else {
                serde_json::json!({"error": "invalid_grant"})
            };
            forms.push(form);
            answer(stream, body).await;
        }
        forms
    });
    (url, served)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_oauth_service_signs_in_through_the_browser_and_refreshes_for_tasks() {
    let (token_url, served) = token_endpoint().await;
    // Added services must use https; this one is written as if added, with
    // its token endpoint on this computer.
    let saved = Saved {
        added: vec![Profile {
            id: "mail".into(),
            name: "Mail".into(),
            method: Method::Oauth {
                authorize_url: "https://auth.example.com/authorize".into(),
                token_url,
                revoke_url: None,
                scopes: vec!["openid".into(), "email".into()],
                variable: "MAIL_TOKEN".into(),
                google: true,
            },
            manage_url: Some("https://auth.example.com/apps".into()),
            builtin: false,
        }],
        ..Saved::default()
    };
    let f = Fixture::with(Duration::from_secs(20), saved);
    assert_eq!(f.account("mail").await.needs, Some(AccountNeed::Client));
    assert!(matches!(
        f.act(AccountAction::SetKey {
            account_id: "mail".into(),
            key: Secret::new("not json"),
        })
        .await,
        Err(AccountsError::Invalid(_))
    ));
    f.act(AccountAction::SetKey {
        account_id: "mail".into(),
        key: Secret::new(
            r#"{"installed":{"client_id":"123.apps.example.com","client_secret":"ClientSecret0123"}}"#,
        ),
    })
    .await
    .unwrap();
    assert_eq!(f.account("mail").await.needs, None);

    f.act(AccountAction::SignIn {
        account_id: "mail".into(),
    })
    .await
    .unwrap();
    f.until_stage(|s| {
        *s == SignInStage::Browser {
            url_host: "auth.example.com".into(),
        }
    })
    .await;
    let opened = url::Url::parse(&f.opener.opened()[0]).unwrap();
    let query: HashMap<String, String> = opened.query_pairs().into_owned().collect();
    assert_eq!(query["client_id"], "123.apps.example.com");
    assert_eq!(query["code_challenge_method"], "S256");
    let redirect = &query["redirect_uri"];
    assert!(redirect.starts_with("http://127.0.0.1:"));

    // Something else asking first is turned away; the browser coming back is not.
    let http = http();
    let stranger = http
        .get(format!("{redirect}?code=x&state=wrong"))
        .send()
        .await
        .unwrap();
    assert_eq!(stranger.status(), 404);
    let back = http
        .get(format!("{redirect}?code=TheCode&state={}", query["state"]))
        .send()
        .await
        .unwrap();
    assert_eq!(back.status(), 200);
    let done = f
        .until_stage(|s| matches!(s, SignInStage::Done { .. }))
        .await;
    assert_eq!(
        done,
        SignInStage::Done {
            identity: Some("owner@example.com".into())
        }
    );
    assert_eq!(f.account("mail").await.state, AccountState::Active);

    // A task gets the access token and a credentials file only it can read.
    let task = TaskId::new();
    let given = f
        .accounts
        .for_run(HarnessKind::ClaudeCode, task)
        .await
        .unwrap();
    assert_eq!(
        env_of(&given, "MAIL_TOKEN").as_deref(),
        Some("ya29.FirstAccess0123")
    );
    let file = PathBuf::from(env_of(&given, GOOGLE_CREDENTIALS).unwrap());
    let credentials = std::fs::read_to_string(&file).unwrap();
    assert!(credentials.contains("1//Refresh0123456789"));
    assert_eq!(
        std::fs::metadata(&file).unwrap().permissions().mode() & 0o077,
        0
    );
    for secret in [
        "ya29.FirstAccess0123",
        "1//Refresh0123456789",
        "ClientSecret0123",
    ] {
        assert!(given.secrets.iter().any(|s| s == secret), "{secret}");
    }
    f.accounts.run_ended(task);
    assert!(!file.exists(), "the file goes when the run ends");

    // A check refreshes; the token the next task gets is the new one.
    f.accounts.check(Some("mail")).await;
    assert_eq!(
        f.accounts.token("mail").await.unwrap().as_str(),
        "ya29.Refreshed0123"
    );
    assert!(!f.written().contains("Refresh0123456789"));
    assert!(!f.written().contains("ClientSecret0123"));

    f.act(AccountAction::Delete {
        account_id: "mail".into(),
    })
    .await
    .unwrap();
    assert!(f.vault.is_empty());
    assert_eq!(
        f.opener.opened().last().map(String::as_str),
        Some("https://auth.example.com/apps")
    );
    drop(f);
    let forms = served.await.unwrap();
    assert!(forms[0].contains("code=TheCode"));
    assert!(forms[0].contains("code_verifier="));
    assert!(forms[1].contains("grant_type=refresh_token"));
}

#[tokio::test]
async fn a_task_renews_only_the_accounts_it_was_given() {
    let f = Fixture::new();
    f.act(AccountAction::AddService {
        service: key_service("Groq", "GROQ_API_KEY"),
    })
    .await
    .unwrap();
    f.act(AccountAction::SetKey {
        account_id: "groq".into(),
        key: Secret::new("gsk_Value0123456789"),
    })
    .await
    .unwrap();
    f.until_state("groq", AccountState::Active).await;
    let task = TaskId::new();
    f.accounts.for_run(HarnessKind::Codex, task).await.unwrap();
    assert_eq!(
        f.accounts.renew(task, "Groq").await.unwrap(),
        "gsk_Value0123456789"
    );
    // GitHub is signed out, so the task wasn't given it; another task wasn't
    // given anything.
    assert!(matches!(
        f.accounts.renew(task, "github").await,
        Err(AccountsError::NotFound(_))
    ));
    assert!(matches!(
        f.accounts.renew(TaskId::new(), "groq").await,
        Err(AccountsError::NotFound(_))
    ));
    // Paused, the account renews for nobody.
    f.act(AccountAction::Pause {
        account_id: "groq".into(),
    })
    .await
    .unwrap();
    assert!(matches!(
        f.accounts.renew(task, "groq").await,
        Err(AccountsError::NotReady(_))
    ));
    f.act(AccountAction::Resume {
        account_id: "groq".into(),
    })
    .await
    .unwrap();
    f.accounts.run_ended(task);
    assert!(matches!(
        f.accounts.renew(task, "groq").await,
        Err(AccountsError::NotFound(_))
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refresh_answered_after_sign_out_keeps_nothing() {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let token_url = format!("http://{}/token", listener.local_addr().unwrap());
    let (asked, asked_rx) = tokio::sync::oneshot::channel();
    let (release, released) = tokio::sync::oneshot::channel::<()>();
    let served = tokio::spawn(async move {
        let mut stream = accept(&listener).await;
        read_form(&mut stream).await;
        asked.send(()).unwrap();
        released.await.unwrap();
        answer(
            stream,
            serde_json::json!({
                "access_token": "ya29.Late0123456789",
                "expires_in": 3600,
                "refresh_token": "1//Rotated0123456789",
            }),
        )
        .await;
    });
    let saved = Saved {
        added: vec![Profile {
            id: "mail".into(),
            name: "Mail".into(),
            method: Method::Oauth {
                authorize_url: "https://auth.example.com/authorize".into(),
                token_url,
                revoke_url: None,
                scopes: vec!["email".into()],
                variable: "MAIL_TOKEN".into(),
                google: false,
            },
            manage_url: None,
            builtin: false,
        }],
        ..Saved::default()
    };
    let f = Fixture::with(Duration::from_secs(20), saved);
    f.act(AccountAction::SetKey {
        account_id: "mail".into(),
        key: Secret::new(
            r#"{"installed":{"client_id":"123.apps.example.com","client_secret":"ClientSecret0123"}}"#,
        ),
    })
    .await
    .unwrap();
    f.vault
        .set("mail", Item::Refresh, "1//Refresh0123456789")
        .await
        .unwrap();

    // The check's refresh is answered only after the sign-out has finished.
    let signing_out = async {
        asked_rx.await.unwrap();
        f.act(AccountAction::SignOut {
            account_id: "mail".into(),
        })
        .await
        .unwrap();
        release.send(()).unwrap();
    };
    tokio::join!(f.accounts.check(Some("mail")), signing_out);
    served.await.unwrap();
    assert!(f.vault.get("mail", Item::Refresh).await.unwrap().is_none());
    assert_eq!(f.account("mail").await.state, AccountState::SignedOut);
    assert!(f.accounts.token("mail").await.is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_sign_out_ends_the_sign_in_going_first() {
    let f = Fixture::new();
    f.act(AccountAction::SignIn {
        account_id: "github".into(),
    })
    .await
    .unwrap();
    f.until_stage(|s| matches!(s, SignInStage::Code { .. }))
        .await;
    f.act(AccountAction::SignOut {
        account_id: "github".into(),
    })
    .await
    .unwrap();
    // The sign-in had ended before the sign-out went on, and writes nothing after.
    assert_eq!(
        f.accounts.view().await.sign_in.map(|s| s.stage),
        Some(SignInStage::Cancelled)
    );
    f.go();
    assert_eq!(f.account("github").await.state, AccountState::SignedOut);
    assert!(!f.dir().join("config").join("github").exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_login_that_exits_with_an_error_is_not_a_sign_in() {
    let f = Fixture::new();
    f.script(
        "claude",
        &CLAUDE.replace(
            r#"touch "$HOME/claude-in" ;;"#,
            r#"touch "$HOME/claude-in"; exit 1 ;;"#,
        ),
    );
    f.act(AccountAction::SignIn {
        account_id: "claude".into(),
    })
    .await
    .unwrap();
    f.go();
    let SignInStage::Failed { reason } = f
        .until_stage(|s| matches!(s, SignInStage::Failed { .. }))
        .await
    else {
        unreachable!()
    };
    assert!(reason.contains("stopped before it finished"), "{reason}");
    // What the tool says now is shown all the same.
    assert_eq!(f.account("claude").await.state, AccountState::Active);
}

#[tokio::test]
async fn a_sign_out_or_delete_that_fails_says_so_and_keeps_the_account() {
    let f = Fixture::new();
    std::fs::write(f.root.path().join("home").join("claude-in"), "").unwrap();
    f.accounts.check(Some("claude")).await;
    assert_eq!(f.account("claude").await.state, AccountState::Active);
    f.script(
        "claude",
        &CLAUDE.replace(
            r#""auth logout") rm -f "$HOME/claude-in" ;;"#,
            r#""auth logout") exit 1 ;;"#,
        ),
    );
    let failed = f
        .act(AccountAction::SignOut {
            account_id: "claude".into(),
        })
        .await
        .unwrap_err();
    assert!(matches!(failed, AccountsError::Failed(_)));
    assert!(failed.to_string().contains("did not sign out"), "{failed}");
    assert_eq!(f.account("claude").await.state, AccountState::Active);
    let history = f.accounts.view().await.history;
    assert_eq!(history[0].event, AccountEvent::SignedOut);
    assert_eq!(
        history[0].detail.as_deref(),
        Some(failed.to_string().as_str())
    );

    // An added login whose folder can't be removed is kept, to try again.
    f.act(AccountAction::AddService {
        service: ServiceProfile {
            name: "Cloud".into(),
            service: ServiceKind::Cli {
                sign_in: "gh auth login".into(),
                status: "gh auth status".into(),
                sign_out: None,
                config_variable: "CLOUD_CONFIG".into(),
                manage_url: Some("https://example.com/cloud".into()),
            },
        },
    })
    .await
    .unwrap();
    let folder = f.dir().join("config").join("cloud");
    std::fs::create_dir_all(folder.join("inner")).unwrap();
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o500)).unwrap();
    let failed = f
        .act(AccountAction::Delete {
            account_id: "cloud".into(),
        })
        .await;
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(matches!(failed, Err(AccountsError::Failed(_))));
    f.account("cloud").await;
    assert!(
        f.opener.opened().is_empty(),
        "nothing is opened for a failed delete"
    );
    f.act(AccountAction::Delete {
        account_id: "cloud".into(),
    })
    .await
    .unwrap();
    let view = f.accounts.view().await;
    assert!(view.accounts.iter().all(|a| a.account_id != "cloud"));
    assert_eq!(f.opener.opened(), ["https://example.com/cloud"]);
}

#[tokio::test]
async fn nothing_is_reported_removed_while_the_keyring_holding_it_is_away() {
    let dir = tempfile::tempdir().unwrap();
    // Never used the keyring here: the file is the whole store.
    let file = vault::without_keyring(dir.path());
    file.set("groq", Item::Key, "gsk_File0123456789")
        .await
        .unwrap();
    file.remove("groq", Item::Key).await.unwrap();
    // Used it before: removing reaches the file but is not done.
    std::fs::write(dir.path().join("keyring-used"), b"").unwrap();
    let away = vault::without_keyring(dir.path());
    away.set("groq", Item::Key, "gsk_File0123456789")
        .await
        .unwrap();
    let refused = away.remove("groq", Item::Key).await.unwrap_err();
    assert!(refused.0.contains("Unlock it"), "{refused}");
    assert!(away.get("groq", Item::Key).await.unwrap().is_none());
    assert!(away.clear().await.is_err());
}

#[tokio::test]
async fn the_keyring_is_not_used_unless_noted_as_used() {
    let dir = tempfile::tempdir().unwrap();
    let accounts = dir.path().join("accounts");
    std::fs::create_dir(&accounts).unwrap();
    std::fs::set_permissions(&accounts, std::fs::Permissions::from_mode(0o500)).unwrap();
    // The note can't be written, so the stand-in keyring is left alone.
    let keyring = Arc::new(MemoryVault::default());
    let vault = vault::with_keyring(&accounts, keyring.clone());
    assert!(vault.note().unwrap().contains("not using the keyring"));
    assert!(
        vault
            .set("groq", Item::Key, "gsk_File0123456789")
            .await
            .is_err()
    );
    assert!(keyring.is_empty());
    std::fs::set_permissions(&accounts, std::fs::Permissions::from_mode(0o700)).unwrap();

    // Noted, the keyring is used, and a later start without it refuses to
    // call anything removed.
    let vault = vault::with_keyring(&accounts, keyring.clone());
    vault
        .set("groq", Item::Key, "gsk_Ring0123456789")
        .await
        .unwrap();
    assert!(!keyring.is_empty());
    let away = vault::without_keyring(&accounts);
    assert!(away.remove("groq", Item::Key).await.is_err());
}

#[tokio::test]
async fn deleting_an_account_whose_keyring_is_away_keeps_it_and_says_why() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("keyring-used"), b"").unwrap();
    let f = Fixture::with_vault(
        Duration::from_secs(20),
        Saved::default(),
        Some(vault::without_keyring(dir.path())),
    );
    f.act(AccountAction::SetKey {
        account_id: "openrouter".into(),
        key: Secret::new("sk-or-v1-Away0123456789"),
    })
    .await
    .unwrap();
    let refused = f
        .act(AccountAction::Delete {
            account_id: "openrouter".into(),
        })
        .await
        .unwrap_err();
    assert!(refused.to_string().contains("Unlock it"), "{refused}");
    assert!(f.opener.opened().is_empty());
}

#[tokio::test]
async fn keys_left_in_the_file_are_read_and_go_with_the_keyring() {
    let dir = tempfile::tempdir().unwrap();
    let both = vault::WithLeftovers::new(
        MemoryVault::default(),
        vault::FileVault::new(dir.path().join("vault")),
    );
    both.file
        .set("groq", Item::Key, "gsk_Left0123456789")
        .await
        .unwrap();
    both.file
        .set("mail", Item::Refresh, "1//Left0123456789")
        .await
        .unwrap();
    assert_eq!(
        both.get("groq", Item::Key).await.unwrap().unwrap().as_str(),
        "gsk_Left0123456789"
    );
    // Kept anew, a value lives in the keyring only.
    both.set("groq", Item::Key, "gsk_New0123456789")
        .await
        .unwrap();
    assert!(both.file.get("groq", Item::Key).await.unwrap().is_none());
    assert_eq!(
        both.get("groq", Item::Key).await.unwrap().unwrap().as_str(),
        "gsk_New0123456789"
    );
    // A copy kept in the file while the keyring was away is the newer one.
    both.file
        .set("groq", Item::Key, "gsk_Newer0123456789")
        .await
        .unwrap();
    assert_eq!(
        both.get("groq", Item::Key).await.unwrap().unwrap().as_str(),
        "gsk_Newer0123456789"
    );
    assert!(both.file.get("groq", Item::Key).await.unwrap().is_none());
    assert_eq!(
        both.keyring
            .get("groq", Item::Key)
            .await
            .unwrap()
            .unwrap()
            .as_str(),
        "gsk_Newer0123456789"
    );
    // Removed, a leftover can't come back.
    both.remove("mail", Item::Refresh).await.unwrap();
    assert!(both.get("mail", Item::Refresh).await.unwrap().is_none());
    both.file
        .set("mail", Item::Client, "{\"client_id\":\"x\"}")
        .await
        .unwrap();
    both.clear().await.unwrap();
    assert!(both.keyring.is_empty());
    assert!(!dir.path().join("vault").exists());
}

/// A token endpoint answering each request with the next of `bodies`; each
/// answer waits for its `release`, after saying it was asked.
async fn held_endpoint(
    bodies: Vec<serde_json::Value>,
) -> (
    String,
    tokio::sync::mpsc::UnboundedReceiver<()>,
    tokio::sync::mpsc::UnboundedSender<()>,
) {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let url = format!("http://{}/token", listener.local_addr().unwrap());
    let (asked, asked_rx) = tokio::sync::mpsc::unbounded_channel();
    let (release, mut released) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        for body in bodies {
            let mut stream = accept(&listener).await;
            read_form(&mut stream).await;
            let _ = asked.send(());
            if released.recv().await.is_none() {
                return;
            }
            answer(stream, body).await;
        }
    });
    (url, asked_rx, release)
}

/// An added OAuth service whose client and refresh token were kept before
/// Accounts started, as after a restart.
async fn restarted_oauth(token_url: String) -> Fixture {
    let f = Fixture::with(Duration::from_secs(20), oauth_saved(token_url));
    let client = r#"{"client_id":"123.apps.example.com","client_secret":"ClientSecret0123"}"#;
    f.vault.set("mail", Item::Client, client).await.unwrap();
    f.vault
        .set("mail", Item::Refresh, "1//Refresh0123456789")
        .await
        .unwrap();
    f
}

/// What [`restarted_oauth`] saved.
fn oauth_saved(token_url: String) -> Saved {
    Saved {
        added: vec![Profile {
            id: "mail".into(),
            name: "Mail".into(),
            method: Method::Oauth {
                authorize_url: "https://auth.example.com/authorize".into(),
                token_url,
                revoke_url: None,
                scopes: vec!["email".into()],
                variable: "MAIL_TOKEN".into(),
                google: false,
            },
            manage_url: None,
            builtin: false,
        }],
        accounts: [(
            "mail".to_owned(),
            Kept {
                status: Status::SignedIn,
                client_set: true,
                ..Kept::default()
            },
        )]
        .into(),
        ..Saved::default()
    }
}

/// A token endpoint whose answer, sent in chunks with no length, goes on
/// until it is no longer read; it says how much it sent.
async fn endless_endpoint() -> (String, tokio::task::JoinHandle<usize>) {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let url = format!("http://{}/token", listener.local_addr().unwrap());
    let served = tokio::spawn(async move {
        let mut stream = accept(&listener).await;
        read_form(&mut stream).await;
        let head = "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\
                    transfer-encoding: chunked\r\n\r\n";
        let _ = stream.write_all(head.as_bytes()).await;
        let _ = stream.write_all(b"5\r\n{\"a\":\r\n").await;
        let chunk = format!("8000\r\n{}\r\n", "a".repeat(0x8000));
        // At most 128 MiB, so the test ends even if all of it is read.
        let mut sent = 0;
        while sent < 128 << 20 && stream.write_all(chunk.as_bytes()).await.is_ok() {
            sent += chunk.len();
        }
        sent
    });
    (url, served)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_token_answer_longer_than_ophio_reads_gives_nothing() {
    let (url, served) = endless_endpoint().await;
    let f = restarted_oauth(url).await;
    let refused = f.accounts.token("mail").await.unwrap_err();
    assert!(
        refused.to_string().contains("longer than Ophio reads"),
        "{refused}"
    );
    // Read no further than the limit: the rest is what the connection held.
    let sent = served.await.unwrap();
    assert!(sent < 32 << 20, "{sent} bytes were read");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn what_ophio_reads_is_masked_and_stays_masked_while_it_is_kept() {
    let (url, mut asked, release) = held_endpoint(vec![
        // Not rotated: the refresh token is only ever read from the vault.
        serde_json::json!({"access_token": "ya29.Fresh0123456789", "expires_in": 3600}),
        // Too short to mask, asked by `ophio token` and by a task starting.
        serde_json::json!({"access_token": "abc1234", "expires_in": 3600}),
        serde_json::json!({"access_token": "abc1234", "expires_in": 3600}),
    ])
    .await;
    let f = restarted_oauth(url).await;
    for _ in 0..3 {
        release.send(()).unwrap();
    }
    assert_eq!(
        f.accounts.token("mail").await.unwrap().as_str(),
        "ya29.Fresh0123456789"
    );
    asked.recv().await.unwrap();
    for held in [
        "ya29.Fresh0123456789",
        "1//Refresh0123456789",
        "ClientSecret0123",
    ] {
        assert!(f.accounts.secrets().iter().any(|s| s == held), "{held}");
    }

    // A token masking would miss is given to nobody.
    f.accounts.lock().tokens.clear();
    let short = f.accounts.token("mail").await.err().unwrap();
    assert!(short.to_string().contains("can't hide"), "{short}");
    let task = TaskId::new();
    let given = f
        .accounts
        .for_run(HarnessKind::ClaudeCode, task)
        .await
        .unwrap();
    assert!(env_of(&given, "MAIL_TOKEN").is_none());
    assert!(!f.accounts.secrets().iter().any(|s| s == "abc1234"));

    // Many tokens renewed later, the ones still kept are masked; of those
    // replaced, the newest are.
    for n in 0..2 * REMEMBERED {
        f.accounts
            .lock()
            .hold("other", ACCESS, &format!("renewed-{n:06}"), None);
    }
    let secrets = f.accounts.secrets();
    for kept in ["1//Refresh0123456789", "ClientSecret0123"] {
        assert!(secrets.iter().any(|s| s == kept), "{kept}");
    }
    assert!(secrets.iter().any(|s| s == "renewed-001500"));
    assert!(!secrets.iter().any(|s| s == "renewed-000000"));
    assert!(secrets.len() <= REMEMBERED + f.accounts.lock().held.len());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pausing_or_ending_while_a_token_is_fetched_gives_nothing() {
    let refreshed = || serde_json::json!({"access_token": "ya29.Late0123456789", "expires_in": 60});
    let (url, mut asked, release) =
        held_endpoint(vec![refreshed(), refreshed(), refreshed()]).await;
    let f = restarted_oauth(url).await;
    let pause = |paused: bool| {
        let action = if paused {
            AccountAction::Pause {
                account_id: "mail".into(),
            }
        } else {
            AccountAction::Resume {
                account_id: "mail".into(),
            }
        };
        f.act(action)
    };

    // `ophio token` asked before the pause.
    let fetching = async {
        asked.recv().await.unwrap();
        pause(true).await.unwrap();
        release.send(()).unwrap();
    };
    let (got, ()) = tokio::join!(f.accounts.token("mail"), fetching);
    assert!(matches!(got, Err(AccountsError::NotReady(_))));
    pause(false).await.unwrap();

    // A task starting before the pause.
    f.accounts.lock().tokens.clear();
    let starting = async {
        asked.recv().await.unwrap();
        pause(true).await.unwrap();
        release.send(()).unwrap();
    };
    let (given, ()) = tokio::join!(
        f.accounts.for_run(HarnessKind::ClaudeCode, TaskId::new()),
        starting
    );
    assert!(env_of(&given.unwrap(), "MAIL_TOKEN").is_none());
    pause(false).await.unwrap();

    // A task given the account and ending while it renews it.
    let task = TaskId::new();
    f.accounts.lock().runs.insert(task, vec!["mail".into()]);
    f.accounts.lock().tokens.clear();
    let ending = async {
        asked.recv().await.unwrap();
        f.accounts.run_ended(task);
        release.send(()).unwrap();
    };
    let (renewed, ()) = tokio::join!(f.accounts.renew(task, "mail"), ending);
    assert!(matches!(renewed, Err(AccountsError::NotFound(_))));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nothing_starts_on_an_account_while_it_is_signed_out() {
    let f = Fixture::new();
    let dir = f.dir().join("config").join("github");
    files::private_dir(&dir).unwrap();
    std::fs::write(dir.join("hosts.yml"), "user: octo\n").unwrap();
    // Its own sign-out waits to be let go.
    f.script(
        "gh",
        &GH.replace(
            r#""auth logout") rm -f "$state" ;;"#,
            r#""auth logout") while [ ! -e "$TEST_GO" ]; do sleep 0.05; done; rm -f "$state" ;;"#,
        ),
    );
    let signing_out = f.act(AccountAction::SignOut {
        account_id: "github".into(),
    });
    let meanwhile = async {
        while !f.accounts.lock().stopping.contains("github") {
            tokio::task::yield_now().await;
        }
        let refused = f
            .act(AccountAction::SignIn {
                account_id: "github".into(),
            })
            .await;
        assert!(
            matches!(refused, Err(AccountsError::Busy(_))),
            "{refused:?}"
        );
        assert!(matches!(
            f.act(AccountAction::SignOut {
                account_id: "github".into(),
            })
            .await,
            Err(AccountsError::Busy(_))
        ));
        f.go();
    };
    let (out, ()) = tokio::join!(signing_out, meanwhile);
    out.unwrap();
    assert!(f.accounts.lock().stopping.is_empty());
    assert_eq!(f.account("github").await.state, AccountState::SignedOut);
    assert!(!dir.exists());
}

/// A vault that keeps each value at once, where a task could read it, then
/// answers only when let go.
#[derive(Debug, Clone)]
struct SlowVault {
    inner: Arc<MemoryVault>,
    wrote: tokio::sync::mpsc::UnboundedSender<()>,
    answer: Arc<tokio::sync::Semaphore>,
    fails: bool,
}
impl SlowVault {
    fn new(fails: bool) -> (Self, tokio::sync::mpsc::UnboundedReceiver<()>) {
        let (wrote, written) = tokio::sync::mpsc::unbounded_channel();
        let vault = Self {
            inner: Arc::default(),
            wrote,
            answer: Arc::new(tokio::sync::Semaphore::new(0)),
            fails,
        };
        (vault, written)
    }
}
#[async_trait::async_trait]
impl Vault for SlowVault {
    fn name(&self) -> String {
        "Slow".into()
    }
    async fn get(
        &self,
        account: &str,
        item: Item,
    ) -> Result<Option<Zeroizing<String>>, vault::VaultError> {
        self.inner.get(account, item).await
    }
    async fn set(&self, account: &str, item: Item, value: &str) -> Result<(), vault::VaultError> {
        self.inner.set(account, item, value).await?;
        let _ = self.wrote.send(());
        self.answer.acquire().await.unwrap().forget();
        if self.fails {
            return Err(vault::VaultError(
                "It answered late, and with an error".into(),
            ));
        }
        Ok(())
    }
    async fn remove(&self, account: &str, item: Item) -> Result<(), vault::VaultError> {
        self.inner.remove(account, item).await
    }
    async fn clear(&self) -> Result<(), vault::VaultError> {
        self.inner.clear().await
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_new_key_is_masked_before_it_is_kept() {
    for fails in [false, true] {
        let (slow, mut written) = SlowVault::new(fails);
        let f = Fixture::with_vault(
            Duration::from_secs(20),
            Saved::default(),
            Some(Arc::new(slow.clone())),
        );
        f.act(AccountAction::AddService {
            service: key_service("Groq", "GROQ_API_KEY"),
        })
        .await
        .unwrap();
        let setting = f.act(AccountAction::SetKey {
            account_id: "groq".into(),
            key: Secret::new("gsk_New0123456789"),
        });
        let meanwhile = async {
            written.recv().await.unwrap();
            // Readable in the vault, so already masked.
            assert!(
                f.accounts
                    .secrets()
                    .iter()
                    .any(|s| s == "gsk_New0123456789")
            );
            slow.answer.add_permits(1);
        };
        let (set, ()) = tokio::join!(setting, meanwhile);
        assert_eq!(set.is_err(), fails);
        // Still masked: kept, or perhaps kept though the vault said it failed.
        assert!(
            f.accounts
                .secrets()
                .iter()
                .any(|s| s == "gsk_New0123456789")
        );
        assert!(
            !f.accounts
                .lock()
                .held
                .keys()
                .any(|(_, what)| *what == INCOMING)
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_move_from_the_file_cannot_undo_a_removal() {
    let dir = tempfile::tempdir().unwrap();
    let (slow, mut written) = SlowVault::new(false);
    let both = Arc::new(vault::WithLeftovers::new(
        slow.clone(),
        vault::FileVault::new(dir.path().join("vault")),
    ));
    both.file
        .set("groq", Item::Key, "gsk_Left0123456789")
        .await
        .unwrap();
    // A check reads the leftover and is moving it into the keyring.
    let reading = tokio::spawn({
        let both = both.clone();
        async move { both.get("groq", Item::Key).await }
    });
    written.recv().await.unwrap();
    // The account is deleted meanwhile.
    let removing = tokio::spawn({
        let both = both.clone();
        async move { both.remove("groq", Item::Key).await }
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!removing.is_finished());
    slow.answer.add_permits(1);
    let read = reading.await.unwrap().unwrap().unwrap();
    assert_eq!(read.as_str(), "gsk_Left0123456789");
    removing.await.unwrap().unwrap();
    assert!(both.get("groq", Item::Key).await.unwrap().is_none());
    assert!(slow.inner.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_account_paused_while_a_later_one_is_fetched_is_not_given() {
    let (url, mut asked, release) = held_endpoint(vec![serde_json::json!({
        "access_token": "ya29.Fresh0123456789", "expires_in": 3600
    })])
    .await;
    let f = restarted_oauth(url).await;
    {
        // Groq comes before Mail, so it is fetched first.
        let mut state = f.accounts.lock();
        state.saved.added.insert(
            0,
            Profile {
                id: "groq".into(),
                name: "Groq".into(),
                method: Method::ApiKey {
                    variable: "GROQ_API_KEY".into(),
                    check_url: None,
                },
                manage_url: None,
                builtin: false,
            },
        );
        state.saved.accounts.insert(
            "groq".into(),
            Kept {
                status: Status::SignedIn,
                ..Kept::default()
            },
        );
    }
    f.vault
        .set("groq", Item::Key, "gsk_Value0123456789")
        .await
        .unwrap();
    let task = TaskId::new();
    let starting = f.accounts.for_run(HarnessKind::ClaudeCode, task);
    let meanwhile = async {
        asked.recv().await.unwrap();
        f.act(AccountAction::Pause {
            account_id: "groq".into(),
        })
        .await
        .unwrap();
        release.send(()).unwrap();
    };
    let (given, ()) = tokio::join!(starting, meanwhile);
    let given = given.unwrap();
    assert!(env_of(&given, "GROQ_API_KEY").is_none());
    assert_eq!(
        env_of(&given, "MAIL_TOKEN").as_deref(),
        Some("ya29.Fresh0123456789")
    );
    // Masked all the same: the task might have reached it some other way.
    assert!(given.secrets.iter().any(|s| s == "gsk_Value0123456789"));
    assert!(!f.accounts.lock().runs[&task].contains(&"groq".to_owned()));
    assert!(matches!(
        f.accounts.renew(task, "groq").await,
        Err(AccountsError::NotFound(_))
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refreshes_of_one_account_take_turns() {
    // A provider that rotates refresh tokens refuses a second use of one.
    let (url, mut asked, release) = held_endpoint(vec![
        serde_json::json!({
            "access_token": "ya29.Fresh0123456789",
            "refresh_token": "1//Rotated0123456789",
            "expires_in": 3600
        }),
        serde_json::json!({"error": "invalid_grant"}),
    ])
    .await;
    let f = restarted_oauth(url).await;
    release.send(()).unwrap();
    release.send(()).unwrap();
    let both = async { tokio::join!(f.accounts.token("mail"), f.accounts.check(Some("mail"))) };
    let (token, ()) = tokio::time::timeout(Duration::from_secs(10), both)
        .await
        .expect("the refreshes did not end");
    assert_eq!(token.unwrap().as_str(), "ya29.Fresh0123456789");
    asked.recv().await.unwrap();
    assert!(asked.try_recv().is_err());
    assert_eq!(f.account("mail").await.state, AccountState::Active);
    assert_eq!(
        f.vault
            .get("mail", Item::Refresh)
            .await
            .unwrap()
            .unwrap()
            .as_str(),
        "1//Rotated0123456789"
    );
}

/// A vault that refuses to keep anything while `refusing`.
#[derive(Debug, Default)]
struct RefusingVault {
    inner: MemoryVault,
    refusing: std::sync::atomic::AtomicBool,
}
#[async_trait::async_trait]
impl Vault for RefusingVault {
    fn name(&self) -> String {
        "Refusing".into()
    }
    async fn get(
        &self,
        account: &str,
        item: Item,
    ) -> Result<Option<Zeroizing<String>>, vault::VaultError> {
        self.inner.get(account, item).await
    }
    async fn set(&self, account: &str, item: Item, value: &str) -> Result<(), vault::VaultError> {
        if self.refusing.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(vault::VaultError("The keyring is locked".into()));
        }
        self.inner.set(account, item, value).await
    }
    async fn remove(&self, account: &str, item: Item) -> Result<(), vault::VaultError> {
        self.inner.remove(account, item).await
    }
    async fn clear(&self) -> Result<(), vault::VaultError> {
        self.inner.clear().await
    }
}

/// A token endpoint answering each request with the next of `bodies`, and
/// the forms it was sent.
async fn recording_endpoint(
    bodies: Vec<serde_json::Value>,
) -> (String, tokio::task::JoinHandle<Vec<String>>) {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let url = format!("http://{}/token", listener.local_addr().unwrap());
    let served = tokio::spawn(async move {
        let mut forms = Vec::new();
        for body in bodies {
            let mut stream = accept(&listener).await;
            forms.push(read_form(&mut stream).await);
            answer(stream, body).await;
        }
        forms
    });
    (url, served)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_renewed_sign_in_the_vault_cannot_keep_is_used_and_kept_later() {
    let (url, served) = recording_endpoint(vec![
        serde_json::json!({
            "access_token": "ya29.First0123456789",
            "refresh_token": "1//Rotated0123456789",
            "expires_in": 3600
        }),
        serde_json::json!({"access_token": "ya29.Second0123456789", "expires_in": 3600}),
    ])
    .await;
    let vault = Arc::new(RefusingVault::default());
    let f = Fixture::with_vault(
        Duration::from_secs(20),
        oauth_saved(url),
        Some(vault.clone()),
    );
    let client = r#"{"client_id":"123.apps.example.com","client_secret":"ClientSecret0123"}"#;
    vault.set("mail", Item::Client, client).await.unwrap();
    vault
        .set("mail", Item::Refresh, "1//Refresh0123456789")
        .await
        .unwrap();

    // The provider renews the refresh token while the keyring is locked.
    vault
        .refusing
        .store(true, std::sync::atomic::Ordering::SeqCst);
    f.accounts.check(Some("mail")).await;
    let AccountState::NeedsAttention { reason } = f.account("mail").await.state else {
        panic!("the unkept sign-in is not said");
    };
    assert!(reason.contains("couldn't be saved"), "{reason}");
    let given = f
        .accounts
        .for_run(HarnessKind::ClaudeCode, TaskId::new())
        .await
        .unwrap();
    assert_eq!(
        env_of(&given, "MAIL_TOKEN").as_deref(),
        Some("ya29.First0123456789")
    );

    // Unlocked: the next check renews with the new one, and keeps it.
    vault
        .refusing
        .store(false, std::sync::atomic::Ordering::SeqCst);
    f.accounts.check(Some("mail")).await;
    assert_eq!(f.account("mail").await.state, AccountState::Active);
    assert_eq!(
        vault
            .get("mail", Item::Refresh)
            .await
            .unwrap()
            .unwrap()
            .as_str(),
        "1//Rotated0123456789"
    );
    let forms = served.await.unwrap();
    assert!(forms[0].contains("refresh_token=1%2F%2FRefresh0123456789"));
    assert!(forms[1].contains("refresh_token=1%2F%2FRotated0123456789"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refresh_answered_after_the_sign_in_is_replaced_keeps_nothing() {
    let id_token = |email: &str| {
        format!(
            "h.{}.s",
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(format!(r#"{{"email":"{email}"}}"#))
        )
    };
    let (url, mut asked, release) = held_endpoint(vec![serde_json::json!({
        "access_token": "ya29.Old0123456789",
        "refresh_token": "1//OldRotated0123456789",
        "id_token": id_token("old@example.com"),
        "expires_in": 3600
    })])
    .await;
    let f = restarted_oauth(url).await;
    let checking = f.accounts.check(Some("mail"));
    let replacing = async {
        asked.recv().await.unwrap();
        // A new OAuth client, as a new sign-in would, replaces what the
        // refresh began with.
        f.act(AccountAction::SetKey {
            account_id: "mail".into(),
            key: Secret::new(
                r#"{"client_id":"456.apps.example.com","client_secret":"OtherSecret0123"}"#,
            ),
        })
        .await
        .unwrap();
        f.vault
            .set("mail", Item::Refresh, "1//NewSignIn0123456789")
            .await
            .unwrap();
        release.send(()).unwrap();
    };
    tokio::join!(checking, replacing);
    assert_eq!(
        f.vault
            .get("mail", Item::Refresh)
            .await
            .unwrap()
            .unwrap()
            .as_str(),
        "1//NewSignIn0123456789"
    );
    let account = f.account("mail").await;
    assert_ne!(account.identity.as_deref(), Some("old@example.com"));
    assert!(!f.accounts.lock().tokens.contains_key("mail"));
}

/// Opens the browser sign-in's page and comes back with a code.
async fn come_back(f: &Fixture) {
    f.until_stage(|s| matches!(s, SignInStage::Browser { .. }))
        .await;
    let opened = url::Url::parse(f.opener.opened().last().unwrap()).unwrap();
    let query: HashMap<String, String> = opened.query_pairs().into_owned().collect();
    let back = http()
        .get(format!(
            "{}?code=TheCode&state={}",
            query["redirect_uri"], query["state"]
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(back.status(), 200);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refusal_answered_after_a_new_sign_in_changes_nothing() {
    // Refreshes wait to be released and are refused; a code is exchanged
    // at once.
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let url = format!("http://{}/token", listener.local_addr().unwrap());
    let (asked_tx, mut asked) = tokio::sync::mpsc::unbounded_channel();
    let release = Arc::new(tokio::sync::Notify::new());
    let released = release.clone();
    tokio::spawn(async move {
        loop {
            let mut stream = accept(&listener).await;
            let (asked, released) = (asked_tx.clone(), released.clone());
            tokio::spawn(async move {
                if read_form(&mut stream)
                    .await
                    .contains("grant_type=refresh_token")
                {
                    let _ = asked.send(());
                    released.notified().await;
                    answer(stream, serde_json::json!({"error": "invalid_grant"})).await;
                } else {
                    answer(
                        stream,
                        serde_json::json!({
                            "access_token": "ya29.NewSignIn0123",
                            "refresh_token": "1//NewSignIn0123456789",
                            "expires_in": 3600
                        }),
                    )
                    .await;
                }
            });
        }
    });
    let f = restarted_oauth(url).await;
    let checking = f.accounts.check(Some("mail"));
    let signing = async {
        asked.recv().await.unwrap();
        f.act(AccountAction::SignIn {
            account_id: "mail".into(),
        })
        .await
        .unwrap();
        come_back(&f).await;
        f.until_stage(|s| matches!(s, SignInStage::Done { .. }))
            .await;
        release.notify_one();
    };
    tokio::join!(checking, signing);
    assert_eq!(f.account("mail").await.state, AccountState::Active);
    let given = f
        .accounts
        .for_run(HarnessKind::ClaudeCode, TaskId::new())
        .await
        .unwrap();
    assert_eq!(
        env_of(&given, "MAIL_TOKEN").as_deref(),
        Some("ya29.NewSignIn0123")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_oauth_client_is_not_replaced_during_its_sign_in() {
    let f = restarted_oauth("http://127.0.0.1:9/token".into()).await;
    f.act(AccountAction::SignIn {
        account_id: "mail".into(),
    })
    .await
    .unwrap();
    f.until_stage(|s| matches!(s, SignInStage::Browser { .. }))
        .await;
    let refused = f
        .act(AccountAction::SetKey {
            account_id: "mail".into(),
            key: Secret::new(
                r#"{"client_id":"456.apps.example.com","client_secret":"OtherSecret0123"}"#,
            ),
        })
        .await
        .unwrap_err();
    assert!(matches!(refused, AccountsError::Busy(_)), "{refused}");
    let client = f.vault.get("mail", Item::Client).await.unwrap().unwrap();
    assert!(client.contains("123.apps.example.com"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn what_a_provider_says_in_error_is_not_shown_or_kept() {
    let (url, served) =
        recording_endpoint(vec![serde_json::json!({"error": "ClientSecret0123"})]).await;
    let f = restarted_oauth(url).await;
    f.act(AccountAction::SignIn {
        account_id: "mail".into(),
    })
    .await
    .unwrap();
    come_back(&f).await;
    let SignInStage::Failed { reason } = f
        .until_stage(|s| matches!(s, SignInStage::Failed { .. }))
        .await
    else {
        unreachable!()
    };
    assert!(!reason.contains("ClientSecret0123"), "{reason}");
    let history = serde_json::to_string(&f.accounts.view().await.history).unwrap();
    assert!(!history.contains("ClientSecret0123"), "{history}");
    assert!(!f.written().contains("ClientSecret0123"));
    served.await.unwrap();
}

#[tokio::test]
async fn a_pause_that_cannot_be_saved_says_so_and_still_holds() {
    let f = Fixture::new();
    f.act(AccountAction::AddService {
        service: key_service("Groq", "GROQ_API_KEY"),
    })
    .await
    .unwrap();
    let dir = f.dir().to_path_buf();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();
    let paused = f
        .act(AccountAction::Pause {
            account_id: "groq".into(),
        })
        .await;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let refused = paused.unwrap_err();
    assert!(
        refused.to_string().contains("restart would undo it"),
        "{refused}"
    );
    assert_eq!(f.account("groq").await.state, AccountState::Paused);
}

#[tokio::test]
async fn accounts_start_paused_when_what_was_saved_cannot_be_read() {
    let f = Fixture::new();
    std::fs::write(f.dir().join("accounts.json"), b"{ damaged").unwrap();
    let again = ComputerAccounts::new(Config {
        dir: f.dir().to_path_buf(),
        vault: f.vault.clone(),
        opener: f.opener.clone(),
        tools: f.accounts.config().tools.clone(),
        http: http(),
        sign_in_limit: Duration::from_secs(5),
    });
    let view = again.view().await;
    assert!(!view.accounts.is_empty());
    assert!(
        view.accounts
            .iter()
            .all(|a| a.state == AccountState::Paused),
        "{:?}",
        view.accounts
    );
    assert_eq!(view.history.len(), 1);
    assert!(f.dir().join("accounts.json.damaged").exists());
    // Kept as paused: a second start doesn't trust anything more.
    let third = ComputerAccounts::new(Config {
        dir: f.dir().to_path_buf(),
        vault: f.vault.clone(),
        opener: f.opener.clone(),
        tools: f.accounts.config().tools.clone(),
        http: http(),
        sign_in_limit: Duration::from_secs(5),
    });
    assert!(
        third
            .view()
            .await
            .accounts
            .iter()
            .all(|a| a.state == AccountState::Paused)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_identity_showing_a_held_secret_is_masked() {
    let id_token = format!(
        "h.{}.s",
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(r#"{"email":"ClientSecret0123@example.com"}"#)
    );
    let (url, served) = recording_endpoint(vec![serde_json::json!({
        "access_token": "ya29.Signed0123456789",
        "refresh_token": "1//Signed0123456789",
        "id_token": id_token,
        "expires_in": 3600
    })])
    .await;
    let mut saved = oauth_saved(url);
    // Kept before the refresh token it shows was read.
    saved.accounts.get_mut("mail").unwrap().identity = Some("1//Refresh0123456789".into());
    let f = Fixture::with(Duration::from_secs(20), saved);
    let client = r#"{"client_id":"123.apps.example.com","client_secret":"ClientSecret0123"}"#;
    f.vault.set("mail", Item::Client, client).await.unwrap();
    f.vault
        .set("mail", Item::Refresh, "1//Refresh0123456789")
        .await
        .unwrap();
    f.accounts.fetch("mail", Item::Refresh).await.unwrap();
    let shown = f.account("mail").await.identity.unwrap();
    assert!(!shown.contains("Refresh0123456789"), "{shown}");

    f.act(AccountAction::SignIn {
        account_id: "mail".into(),
    })
    .await
    .unwrap();
    come_back(&f).await;
    let SignInStage::Done { identity } = f
        .until_stage(|s| matches!(s, SignInStage::Done { .. }))
        .await
    else {
        unreachable!()
    };
    let identity = identity.unwrap();
    assert!(!identity.contains("ClientSecret0123"), "{identity}");
    assert!(identity.ends_with("@example.com"), "{identity}");
    let shown = f.account("mail").await.identity.unwrap();
    assert!(!shown.contains("ClientSecret0123"), "{shown}");
    let history = serde_json::to_string(&f.accounts.view().await.history).unwrap();
    assert!(!history.contains("ClientSecret0123"), "{history}");
    assert!(!f.written().contains("ClientSecret0123"));
    served.await.unwrap();
}

#[tokio::test]
async fn a_recovery_that_could_not_be_saved_still_starts_paused() {
    let f = Fixture::new();
    let start = || {
        ComputerAccounts::new(Config {
            dir: f.dir().to_path_buf(),
            vault: f.vault.clone(),
            opener: f.opener.clone(),
            tools: f.accounts.config().tools.clone(),
            http: http(),
            sign_in_limit: Duration::from_secs(5),
        })
    };
    async fn all_paused(accounts: &ComputerAccounts) -> bool {
        let view = accounts.view().await;
        !view.accounts.is_empty()
            && view
                .accounts
                .iter()
                .all(|a| a.state == AccountState::Paused)
    }
    let file = f.dir().join("accounts.json");
    std::fs::write(&file, b"{ damaged").unwrap();
    // Neither the paused state nor the copy aside can be saved.
    std::fs::set_permissions(f.dir(), std::fs::Permissions::from_mode(0o500)).unwrap();
    let first = start();
    std::fs::set_permissions(f.dir(), std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(all_paused(&first).await);
    assert_eq!(std::fs::read(&file).unwrap(), b"{ damaged");
    // Room again: still paused, and now saved so.
    assert!(all_paused(&start()).await);
    assert!(f.dir().join("accounts.json.damaged").exists());
    assert!(Store::new(f.dir()).load().is_some());
    // The file gone while the copy is aside: what was paused is unknown.
    std::fs::remove_file(&file).unwrap();
    assert!(Store::new(f.dir()).load().is_none());
    assert!(all_paused(&start()).await);
    // Unreadable is not missing.
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o000)).unwrap();
    let unreadable = Store::new(f.dir()).load();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(unreadable.is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_tool_token_read_before_a_new_sign_in_is_not_given() {
    let f = Fixture::new();
    let dir = f.dir().join("config").join("github");
    files::private_dir(&dir).unwrap();
    std::fs::write(dir.join("hosts.yml"), "user: octo\n").unwrap();
    let home = f.root.path().join("home");
    // The first read waits, and gives the token from before.
    f.script(
        "gh",
        &GH.replace(
            r#""auth token") [ -e "$state" ] && printf 'gho_TestToken0123456789\n' || exit 1 ;;"#,
            r#""auth token")
  [ -e "$state" ] || exit 1
  if [ -e "$HOME/hold" ]; then
    rm -f "$HOME/hold"; touch "$HOME/held"
    while [ ! -e "$HOME/release" ]; do sleep 0.05; done
    printf 'gho_Before0123456789\n'
  else
    printf 'gho_After01234567890\n'
  fi ;;"#,
        ),
    );
    std::fs::write(home.join("hold"), "").unwrap();
    let reading = f.accounts.token("github");
    let signing = async {
        while !home.join("held").exists() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        f.act(AccountAction::SignIn {
            account_id: "github".into(),
        })
        .await
        .unwrap();
        f.until_stage(|s| matches!(s, SignInStage::Code { .. }))
            .await;
        f.go();
        f.until_stage(|s| matches!(s, SignInStage::Done { .. }))
            .await;
        std::fs::write(home.join("release"), "").unwrap();
    };
    let (read, ()) = tokio::join!(reading, signing);
    assert_eq!(read.unwrap().as_str(), "gho_After01234567890");
    assert_eq!(
        f.accounts.lock().tokens["github"].value.as_str(),
        "gho_After01234567890"
    );
}

#[tokio::test]
async fn a_paused_account_needing_attention_shows_paused_until_resumed() {
    let reason = "GitHub refused the sign-in.";
    let f = Fixture::with(
        Duration::from_secs(20),
        Saved {
            accounts: [(
                "github".to_owned(),
                Kept {
                    status: Status::Attention {
                        reason: reason.into(),
                    },
                    ..Kept::default()
                },
            )]
            .into(),
            ..Saved::default()
        },
    );
    let attention = AccountState::NeedsAttention {
        reason: reason.into(),
    };
    assert_eq!(f.account("github").await.state, attention);
    let id = || "github".to_owned();
    f.act(AccountAction::Pause { account_id: id() })
        .await
        .unwrap();
    assert_eq!(f.account("github").await.state, AccountState::Paused);
    f.act(AccountAction::Resume { account_id: id() })
        .await
        .unwrap();
    assert_eq!(f.account("github").await.state, attention);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refusal_of_a_replaced_key_changes_nothing() {
    // The old key's check waits to be released and is refused; the new
    // key's is answered at once.
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let url = format!("http://{}/key", listener.local_addr().unwrap());
    let (asked_tx, mut asked) = tokio::sync::mpsc::unbounded_channel();
    let release = Arc::new(tokio::sync::Notify::new());
    let released = release.clone();
    tokio::spawn(async move {
        loop {
            let mut stream = accept(&listener).await;
            let (asked, released) = (asked_tx.clone(), released.clone());
            tokio::spawn(async move {
                let mut head = Vec::new();
                let mut byte = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") && stream.read(&mut byte).await.unwrap() == 1 {
                    head.push(byte[0]);
                }
                let old = String::from_utf8_lossy(&head).contains("OldKey0123456789");
                let _ = asked.send(old);
                let status = if old {
                    released.notified().await;
                    "401 Unauthorized"
                } else {
                    "200 OK"
                };
                let reply =
                    format!("HTTP/1.1 {status}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n");
                stream.write_all(reply.as_bytes()).await.unwrap();
                let _ = stream.shutdown().await;
            });
        }
    });
    let saved = Saved {
        added: vec![Profile {
            id: "keyed".into(),
            name: "Keyed".into(),
            method: Method::ApiKey {
                variable: "KEYED_API_KEY".into(),
                check_url: Some(url),
            },
            manage_url: None,
            builtin: false,
        }],
        accounts: [(
            "keyed".to_owned(),
            Kept {
                status: Status::SignedIn,
                ..Kept::default()
            },
        )]
        .into(),
        ..Saved::default()
    };
    let f = Fixture::with(Duration::from_secs(20), saved);
    f.vault
        .set("keyed", Item::Key, "OldKey0123456789")
        .await
        .unwrap();
    let checking = f.accounts.check(Some("keyed"));
    let replacing = async {
        assert!(asked.recv().await.unwrap());
        f.act(AccountAction::SetKey {
            account_id: "keyed".into(),
            key: Secret::new("NewKey0123456789"),
        })
        .await
        .unwrap();
        // The new key's own check has been answered and kept.
        assert!(!asked.recv().await.unwrap());
        f.until(|v| {
            v.accounts
                .iter()
                .any(|a| a.account_id == "keyed" && a.checked_at.is_some())
                .then_some(())
        })
        .await;
        release.notify_one();
    };
    tokio::join!(checking, replacing);
    assert_eq!(f.account("keyed").await.state, AccountState::Active);
    let given = f
        .accounts
        .for_run(HarnessKind::ClaudeCode, TaskId::new())
        .await
        .unwrap();
    assert_eq!(
        env_of(&given, "KEYED_API_KEY").as_deref(),
        Some("NewKey0123456789")
    );
}

/// Answers each request with the next of `answers`, as (status, body), and
/// returns what was asked: the request line, the authorization and the body.
async fn github_endpoint(
    answers: Vec<(u16, &'static str)>,
) -> (
    String,
    tokio::task::JoinHandle<Vec<(String, String, String)>>,
) {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let served = tokio::spawn(async move {
        let mut asked = Vec::new();
        for (status, body) in answers {
            let mut stream = accept(&listener).await;
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            let (head, length) = loop {
                let n = stream.read(&mut chunk).await.unwrap();
                buf.extend_from_slice(&chunk[..n]);
                let text = String::from_utf8_lossy(&buf).into_owned();
                if let Some(at) = text.find("\r\n\r\n") {
                    let length: usize = text[..at]
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse().unwrap())
                        })
                        .unwrap_or(0);
                    if buf.len() >= at + 4 + length {
                        break (text[..at].to_owned(), length);
                    }
                }
            };
            let text = String::from_utf8_lossy(&buf).into_owned();
            let body_in = text[head.len() + 4..head.len() + 4 + length].to_owned();
            let auth = head
                .lines()
                .find_map(|l| {
                    l.to_ascii_lowercase()
                        .starts_with("authorization:")
                        .then(|| l["authorization:".len()..].trim().to_owned())
                })
                .unwrap_or_default();
            asked.push((head.lines().next().unwrap().to_owned(), auth, body_in));
            let reply = format!(
                "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(reply.as_bytes()).await.unwrap();
            let _ = stream.shutdown().await;
        }
        asked
    });
    (url, served)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn github_calls_go_as_the_signed_in_account_and_say_what_github_refused() {
    use ports::github::{Github, GithubError, Method};
    let f = Fixture::new();
    let (url, served) = github_endpoint(vec![
        (200, r#"{"login":"octo"}"#),
        (
            422,
            r#"{"message":"Validation Failed","errors":[{"message":"A pull request already exists for octo:ophio/7-fix."}]}"#,
        ),
        (200, r#"{"data":null,"errors":[{"message":"Could not resolve to a Repository"}]}"#),
        (200, r#"{"data":{"viewer":{"login":"octo"}}}"#),
        (204, ""),
    ])
    .await;
    let api = github::GithubApi::at(f.accounts.clone(), &url);
    // Signed out: nothing is sent.
    assert_eq!(
        api.rest(Method::Get, "/user", None).await,
        Err(GithubError::NotSignedIn)
    );
    f.act(AccountAction::SignIn {
        account_id: "github".into(),
    })
    .await
    .unwrap();
    f.go();
    f.until_stage(|s| matches!(s, SignInStage::Done { .. }))
        .await;
    assert_eq!(
        api.rest(Method::Get, "/user", None).await.unwrap()["login"],
        "octo"
    );
    assert_eq!(
        api.rest(
            Method::Post,
            "/repos/octo/app/pulls",
            Some(serde_json::json!({"title": "t"}))
        )
        .await,
        Err(GithubError::Status {
            code: 422,
            message: "Validation Failed: A pull request already exists for octo:ophio/7-fix."
                .into()
        })
    );
    assert_eq!(
        api.graphql("query { x }", serde_json::json!({})).await,
        Err(GithubError::Status {
            code: 200,
            message: "Could not resolve to a Repository".into()
        })
    );
    assert_eq!(
        api.graphql("query { viewer { login } }", serde_json::json!({}))
            .await
            .unwrap(),
        serde_json::json!({"viewer": {"login": "octo"}})
    );
    assert_eq!(
        api.rest(Method::Delete, "/repos/octo/app/git/refs/heads/x", None)
            .await,
        Ok(serde_json::Value::Null)
    );
    assert!(matches!(
        api.rest(Method::Get, "//evil.example/x", None).await,
        Err(GithubError::Failed(_))
    ));
    let asked = served.await.unwrap();
    assert_eq!(asked[0].0, "GET /user HTTP/1.1");
    assert_eq!(asked[0].1, "Bearer gho_TestToken0123456789");
    assert_eq!(asked[1].0, "POST /repos/octo/app/pulls HTTP/1.1");
    assert_eq!(asked[1].2, r#"{"title":"t"}"#);
    assert_eq!(asked[2].0, "POST /graphql HTTP/1.1");
    assert_eq!(
        asked[4].0,
        "DELETE /repos/octo/app/git/refs/heads/x HTTP/1.1"
    );
}
