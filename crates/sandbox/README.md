# Conversation sandbox

A Mac's version is `macos.rs` and described at the end. On Linux:

The companion composes `Bubblewrap`, probes real namespace creation and the
installed shim, and advertises `conversation_access_v1` plus an availability
reason. Restricted conversations fail closed if isolation is unavailable.
Ordinary tasks retain their existing launch paths. Fully unrestricted
conversations bypass bubblewrap.

Source reviews started from restricted chats inherit the caller's read and
network grants with writes removed. They run in the conversation sandbox;
their fixes wait for the person to start them. Review folder scopes must name
existing directories relative to the project root, without parent segments,
absolute paths or symlinks leading outside the project.

Install bubblewrap at `/usr/bin/bwrap` and build/install `ophio-sandbox-shim`
alongside `ophio`. The Linux packaging and updater include the shim. On systems
that restrict user namespaces, the service must be allowed to create them;
Ubuntu's AppArmor bwrap profile is one relevant configuration. No unsandboxed
fallback is used. The namespace probe runs in the companion's environment.

## Live changes

Every registered project is a launch candidate. Candidates start as writable
binds, read-only binds, or binds covered by empty read-only tmpfs mounts. The
conversation's own project is always readable. Writes imply reads. Nested
projects mount parent-first, with each child's effective grant inherited from
its parents. Home can be a registered project; private masks remain in force.

The parent holds descriptors for the mount namespace and its **owning** user
namespace, obtained with `NS_GET_USERNS`. Bubblewrap's final user namespace may
be a child of that owner; entering it instead makes mount changes fail with
EPERM. Agent processes have no mount capabilities in the owning namespace.

For disjoint candidates, uncovering a read grant and toggling read/write happen
in the same mount namespace, without replacing the agent or its current turn.
The CONNECT proxy applies internet changes to new and existing tunnels. It
refuses loopback, private and link-local destinations even when internet is on.
Internet off admits only the configured provider hosts, on port 443. Network
access that bypasses the proxy has no route out of the private network namespace.

Read revocation, whole-computer changes, newly registered folders, nested mount
changes and an open writer preventing a read-only remount return `NeedsRestart`.
Services terminate the old containment, wait for its exit, and resume the same
native session with a brief explanation and the new grants. They do not report
these changes as enforced before the command's effect has completed. Suspending
an open writer would not fix EBUSY.

## Boundaries and runtime exceptions

This is filesystem and network isolation, not a VM or a defense against kernel
vulnerabilities. Runtime system directories (`/usr`, libraries, `/etc`, and
package metadata used by executable wrappers), synthetic `/dev` and `/proc`,
temporary space, the conversation's scratch directory, explicitly required
executable files, and its own provider credential file are available in addition
to project grants. A hidden candidate appears empty; its name is not secret.
Read-only mounts prevent writes; they do not prevent an interpreter from reading
and executing code. No semantic claim about arbitrary computation is made.

Ophio data and configuration are masked. Only this run's explicit scratch/files
are restored. Each conversation has isolated provider configuration, databases
and history; global hooks, plugins, MCP configuration and other conversations'
transcripts are not mounted. The person's own instructions and skills
(`src/setup.rs`: Claude Code's `CLAUDE.md`, `rules/` and `skills/` but not
`skills/synced/`; Codex's `AGENTS.md`, `AGENTS.override.md` and `skills/` but
not `.system`) are shared read-only into that configuration: bind mounts on
Linux, links the Seatbelt profile reads through on a Mac. A source is resolved
through at most one link, must be the right kind of thing, and must not be,
hold or sit inside anything private, either agent's settings folder (other
than its own place there), `/`, the home folder or a folder above the working
folder. It is checked by `O_NOFOLLOW` opens and held open for the run; on Linux
the shim compares device and inode at each mount point before it starts the
agent (bubblewrap 0.9 has no `--ro-bind-fd`). Links inside a shared skill are
not followed or mounted. When the run can write nothing, a skill whose front
matter (first 16 KiB) has `allowed-tools` naming a file-editing tool, or
`metadata` `ophio-access: write`, is left out. So is anything the run's
`WrapRequest::disabled` names, holds or leads to (Disable in Øphio): its
agent would find it at the copy's path, which the harness's path-based
exclusions (`claudeMdExcludes`, Codex's `skills.config`) don't match.
The places in the copy are made through handles to `state/native` and its
folders, opened through no link, by single names (`openat`, `unlinkat`,
`symlinkat`, `renameat`): a link the agent leaves in place of a folder, or
swaps in later, is never followed. Only links, empty folders and empty files
are removed, as long as nothing else is writing the folder at that moment;
either way only the conversation's own files can be touched. On Linux an
agent's own folder or file of the same kind as the place stays, hidden under
the read-only mount until sharing stops; a mismatch of kind, and anything at a
place on a Mac, is moved aside to `.ophio-aside-…`.
The Mac's sign-in and history links are made the same way, at any depth: a
link planted where one of their folders should be is replaced by a real
folder, and a failure there refuses the run rather than leaving a link out.
A plain file left at the sign-in link is removed, not moved aside, so no copy
of a sign-in stays in the conversation's folder.
That keeps preparation safe. On Linux the shim checks each mount before the
agent starts, so it sees the person's own item at each place. A Mac has no
such check: a process the agent left behind can replace a link after it was
placed, and the agent then reads its own content under that name. Seatbelt's
real-path rules guarantee only that it can read nothing it wasn't already
allowed to. Names with control characters,
and a settings path that isn't plain text, are refused so the check list
stays exact. Refusals are skipped and logged once, by name only. Only a requested native session is imported, and
only the reported session's transcript is exported after containment ends, so
later unrestricted turns can resume it. Host credential contents are never
inspected or copied by the sandbox.

A receipt leaves out a file the agent writes only in this copy of its
settings folder (`Sandbox::own_settings_file`, decided in `src/own.rs`), since
nothing of the person's changed. The sandbox records, as it builds the run,
which places the agent sees are the conversation's own and where each really
is. A path counts only when it is a plain file there now, with one link,
whose real path is inside the conversation's own state. The sign-in file,
shared items, the transcript that is exported and Codex's `sessions/` stay
listed, as does anything before the sandbox is built. This changes only what
the receipt shows, not what the agent can reach.

Restricted conversations do not receive the companion's host tool bridge.
Provider-hosted search and cloud connectors are disabled: those would execute
outside the host namespace. Local shell network tools use the live proxy policy.
Blocks reported by the proxy and absolute denied paths in native tool results
become `Task.access_blocked` with a project match and `TaskUpserted`. Tool-output
recognition is best effort, not a kernel audit stream.

## Independent project checks

`checks::prepare` creates single-use project and scratch access without agent
sign-ins. Approved internet access is intersected with both the task's saved
and current permissions. A restricted task uses a fresh instance of the same
CONNECT proxy as agent sandboxes: loopback and link-local destinations are
refused, and private networks require the task's additional permission. No
agent proxy, account settings or provider credentials are shared. Linux uses
the installed shim inside a private network namespace. Mac pins the fresh
proxy's TCP port and permits direct loopback only with the task's existing
internet and local-network permissions. Missing isolation or helpers fails closed.
Network-on checks set both `NO_PROXY` and `no_proxy` to
`localhost,127.0.0.1,::1`, so clients do not send local requests to the proxy.

Mac checks, like agents, may bind and accept connections on localhost even
offline. Because Mac loopback is shared with the host, an offline check cannot
call its own local server. Only a task whose agent was genuinely unsandboxed
can authorize other direct IP networking. On Mac this permits outbound IP,
not arbitrary Unix sockets; Unix sockets remain limited to the project and
fresh scratch. Listener rules match the agent's: Seatbelt's `localhost` bind
rule does not prevent binding a nonloopback address. It is not a guarantee
against nonlocal binds. Windows still reports that file and network access
are unsandboxed.

Tool lookup first resolves project pins, then the manager's own global default,
using only installed versions inside existing toolchain grants. Ophio reads the
known settings files itself, without adding settings files to the check's reads.
Existing grants are unchanged: Volta's default already lies inside its approved
tools folder and remains readable there, just as it is for the agent.
Resolved selections precede system folders. Without a usable selection, only
the newest stable version per tool follows the system folders; prerelease and
free-threaded builds are not automatic fallbacks. uv's private Python folders
never enter PATH. Explicit non-manager tools such as Cargo remain available.
Every executable folder is resolved through links inside its original grant;
missing versions, unsupported hints and escaping links fall through without
broadening reads. No shell startup code or manager command is executed.

Every Linux check starts a new session, with or without a systemd user manager.
The transient unit uses control-group cleanup without requiring systemd 250's
`ExitType` property. A launcher refusal is a startup error, not a failed test.
Unix stdout carries a trusted sandboxed startup record before any project code
runs; the wrapper then redirects the check's stdout to stderr. Consequently a
check's printed diagnostics cannot change whether Ophio considers it started.
Preparation, sandbox startup and execution share one overall deadline; there
is no separate five-second startup ceiling. Expiry is a timeout, while an
actual startup-channel failure retains its launch diagnostics. If the systemd
availability probe exhausts the deadline, the runner reports a timeout without
starting either sandbox launcher or project command. Timeout
cleanup also stops the owned launcher before a delayed unit can appear.
On Mac, cleanup signals the owned child's group before reaping even after a
normal exit; recovered identities retain their separate start-time checks.

Project snapshots compare files larger than 5 MiB using metadata, without reading
their contents. On Windows, a same-size, in-place rewrite can go unnoticed if the
file's modification time is restored. Unix change time catches that case; neither
platform claims content verification for metadata-only entries.

## Verification

On Linux, `cargo test -p sandbox` covers policy, history isolation and mount construction.
`cargo test -p sandbox --test live -- --ignored --nocapture` additionally requires
real user namespaces and tests nested/home masks, namespace escape attempts,
open writers and read revocation. Run through `systemd-run --user` where the
calling shell cannot create namespaces.

The services regression checks the sandbox attached to a dispatched review
with native shell reads against synthetic permitted and private files, and
confirms that inherited write access is removed. With user namespaces available:

```sh
cargo build --locked -p sandbox --bin ophio-sandbox-shim
OPHIO_TEST_SANDBOX_SHIM="$CARGO_TARGET_DIR/debug/ophio-sandbox-shim" \
  cargo test --locked -p services --lib \
  a_restricted_review_cannot_read_private_files_with_native_tools -- --ignored
```

Set `CARGO_TARGET_DIR` to an absolute scratch build directory for both commands.
No agent or real GitHub request is made by this test.

The opt-in `midturn` example exercises the actual wrap/prepare/started/update
path with a shell. `agent_midturn` uses the installed Codex with `gpt-6.1-sol`,
or Claude with `--claude`, and uses native subscription quota. The companion's
`sandbox_midturn` example runs the same 18-attempt experiment through actual
composition and `SetConversationAccess`; `--restart` exercises read revocation
and native-session resumption. Restarting ends running shell commands; their
process handles cannot be resumed. Binary paths default beneath `$HOME` and can
be overridden with `OPHIO_LIVE_CODEX` or `OPHIO_LIVE_CLAUDE`. These examples are
not default tests.

Clients must show the availability reason and wait for command observation to
confirm enforcement. The shared phone controls and Allow action are a separate
client change. Windows has no sandbox yet: `NoWindowsSandbox` reports limited chats
as unavailable and refuses to start any chat that has limits.

## On a Mac

`Seatbelt` runs the agent under `/usr/bin/sandbox-exec` with a profile that
denies everything and then gives back what programs need to run, the conversation's
folders, and this run's own files. Later rules win, so private data is taken back
after folders are given, even from a whole-computer read. A Mac has no mount or
network namespaces, which makes six things different from Linux:

- A profile can't change after the program starts. A folder change returns
  `NeedsRestart`. The proxy's policy changes at once, as on Linux, because direct
  network access is denied either way. Adding or taking back direct access to
  this computer's loopback also needs a restart. A rejected or restart-required
  update stays pending; a later stale update cannot acknowledge it as enforced.
- The proxy listens on a loopback TCP port, the one network address the profile
  allows, rather than on a socket file bound inside. There is no shim. Model-only
  runs instead pin a separate relay port, start with fresh provider settings and
  clear inherited credentials. A fresh random local capability authenticates
  relay requests before inspection, audit, usage or run-attributed signals, so
  unrelated host probes do not quarantine the conversation. They cannot reach
  Unix sockets, open a server or gain other network routes through
  internet/local-network grants. Disk budgets remain unsupported and are refused.
- There's one loopback per computer, so an agent can listen but can't connect to
  anything on this Mac unless the conversation has its own-network permission, and
  then it can reach the person's other local servers too. The computer reaches what
  an agent serves directly.
- Nothing can be mounted over the agent's own folder. It runs with `CLAUDE_CONFIG_DIR`
  or `CODEX_HOME` set to the conversation's `native` folder, with links to its
  history and its sign-in file. A sign-in file an agent replaces, rather than writes
  into, is carried back when the run ends. Claude Code keeps its sign-in in the
  login keychain, which a sandboxed program can't reach, so it is copied into that
  folder, owner-only, for the run, and a renewed one goes back to the keychain.
  If Øphio stops before the run ends (a restart, an update, a crash), the next run
  in that conversation carries a renewed sign-in back before it sets up, then is
  given it. For that, `state/sign-in-given`, beside the record of shared places and
  out of the agent's reach, notes which sign-in the run was given, by SHA-256 digest
  only; a run that ends and settles its sign-in removes it. A renewed sign-in goes
  back only over the one the run was given. If the person signed in again
  meanwhile, theirs stays and the renewed one is dropped. If they signed out (the
  keychain has no entry, which `security` reports with exit status 44, or the
  sign-in file was removed, as `codex logout` does), the renewed one is dropped
  too, so it doesn't keep the agent signed in. It is dropped as well with no note,
  or when the agent left a link, a file with a second name, or an empty file
  there, or one too big to be a sign-in (over 64 KiB for the keychain's copy).
  A keychain that can't be read (locked, say) is not a sign-out: nothing is
  dropped and the note stays.
  When the renewed sign-in can't go back yet (the keychain is locked or won't
  take it, or the sign-in file can't be replaced), the run starts anyway with it
  as its own sign-in: the keychain's copy stays in place of a fresh one, and a
  sign-in file stays where its link would be. The note stays as it was, and the
  end of the run tries again under the same rule; until one succeeds, each run in
  that conversation does the same, and Øphio logs a warning with no part of the
  sign-in in it. A run that ends without carrying back a renewal it owes keeps
  its copy and the note in the same way, and says so.
  The keychain's copy goes back through `security`'s own prompt, so it never
  shows in a process list. That prompt cuts a line at 4,096 bytes and runs the
  rest as a command of its own, so a line over 4,032 bytes (Claude Code's own
  margin) isn't sent: a renewed sign-in over about 1,950 bytes is never stored
  cut, and stays with the conversation as above instead. Claude Code itself
  passes a longer one as a command argument.
  Known limit: a renewal left by an unfinished run goes back only when that same
  conversation next starts. A different conversation started first is given the
  sign-in the keychain or file still holds, which may be the older one.
  Claude Code's own temporary files, otherwise in the shared `/tmp/claude-<uid>`,
  go in the run's temporary folder through `CLAUDE_CODE_TMPDIR`.
- The keychain's service is held, since it would hand out the person's keychain,
  and programs that check certificates through the system ask it. Runs get
  `SSL_CERT_FILE` set to the system's own certificates, `/private/etc/ssl/cert.pem`,
  which Codex reads instead. Certificates the person added to the keychain aren't
  trusted inside.
- Other programs of the same person aren't hidden, since a Mac has no process
  namespace. The profile denies asking the system about them (their program
  file, folder, open files and use, which `lsof` and the like read) and gives
  that back only for the run's own programs. It still lets a run list program
  numbers, which `lsof` needs even for its own. Known limit: the command line of
  any of the person's programs, and its short name, can still be read from
  inside. The system answers `kern.procargs2`, `kern.procargs` and
  `kern.proc.pid` without asking the profile, even one that denies every
  `sysctl-read`; tested on macOS 27.2. Its environment isn't given. So a secret
  passed as a command argument, by any of the person's programs, can be read by
  a limited agent while that program runs. Only a different user account would
  close it. `ps`, `top`, `pgrep` and `pkill` don't work inside, as before: `ps`
  and `top` can't start, and `pgrep` and `pkill` ask a system service the
  profile holds.

Path rules leave gaps a mount would not, so the profile also: protects private
folders where they really are, whether or not they exist yet, including Øphio's data
under names not seen yet; refuses renaming any folder above private data, since
moving it would take the data out from under its rule, and refuses making one that
doesn't exist yet, since a link there could lead the data elsewhere; opens only
terminals the run itself made; gives no named shared memory, and named semaphores
only under `/mp-`, the name Python's multiprocessing can't start without. That leaves
one known reach outside: another program's semaphore under that name, if its name is
known. A semaphore carries no data, and Python's own are random and unlinked as soon
as they are made. It also opens the proxy's port by TCP alone, holding it on IPv6 too, because a profile can name only `localhost`. Homebrew
is given as its programs and libraries, not its whole prefix, whose `var` holds
services' data. A write grant over a folder that runs things later, such as a home
folder with `Library/LaunchAgents` or a shell's startup file, is a grant to run
outside the sandbox at the next login, as it is on Linux: only the person can give it.

The Mac process container remains a process group, not an immutable descendant
boundary. A detached session can outlive group cleanup; neither the relay nor
Seatbelt changes that. Aggregate CPU, memory and process limits are refused,
not replaced by per-process limits or a polling monitor. A storage image would
bound only writes confined to that image, not the current writable host grants.
A stronger contract needs separately approved operating-system isolation,
such as a provisioned separate identity/helper or a virtual machine; a separate
identity alone does not provide all aggregate resource limits. Mac relay ports
are pinned on both loopback address families for the relay's lifetime, not after
the sandbox is dropped. This is model-only egress during the run, not complete
process containment or protection against a colluding unsandboxed host process.

`file-read-metadata` is allowed everywhere, since programs look up each folder on
the way to a file: a file's existence and size can be learned by anyone who knows
its exact path. Its contents, and folder listings, can't. The full Mac sandbox
test suite runs real programs in real profiles, including separate sign-in and
keychain fixtures; it is not the account-safe relay selection below. The Claude
Code keychain path has been tested only with a stand-in entry, not a real sign-in.

## Model relay

Set `SandboxSpec.relay` to a `RelaySpec` for runs that read untrusted input.
It selects Anthropic or OpenAI, a host-only key file, request/token/deadline
limits, an optional model allowlist and output-token ceiling, a host-only
JSONL audit file, and an optional `OnRelaySignal` callback. The key file holds
one API key as plain text, with optional surrounding whitespace. The callback
receives `HijackAttempt` or `BudgetSpent`; callers should enqueue quarantine
work without blocking the relay worker. `Sandbox::relay_totals` exposes admitted
requests and charged input/output tokens, including Anthropic cache tokens
and reservations for incomplete requests. Token-count estimates conservatively
consume the input budget too.

This mode replaces the CONNECT proxy. On Linux the same shim forwards namespace
`127.0.0.1:18080` to the relay's Unix socket. On a Mac the relay listens directly
on a per-run loopback TCP port, pins the IPv6 spelling too, and the profile
allows only that port. Mac relay profiles also deny serving connections and
outbound Unix sockets, even in writable scratch folders. There is no other
network route through the sandbox policy, even when internet or local-network
grants are set or later changed. A relay
sandbox starts with fresh provider state, omits `given` account files and host
login state, and rejects native-session resumption and host tool bridges.
Whole-computer grants are refused. Explicit folder, scratch, executable and
visible-file mounts must not overlap the key, audit log, account paths or
provider login folders, including resolved symlinks. Keep the key and audit
log outside the staged source and scratch folders. Stage trusted files without
hard links to credentials; arbitrary existing copies of secrets cannot be
identified from a configured key path.

`Wrapped.clear_env` requires launchers to clear inherited environment and
account overrides before applying `Wrapped.env` and the harness's relay
settings. Both harnesses do this. Only basic runtime variables, the isolated
provider-state location and local relay authentication enter the child: a dummy
value on Linux, a fresh per-run capability on Mac. The latter is supplied only
through the wrapped environment, never command arguments. Harnesses preserve
that exact sandbox-supplied value while still forcing their relay endpoint and
security settings. Runs without a relay retain their existing subscription,
proxy and history behavior.

On Mac the relay requires exactly one matching `x-api-key` (Anthropic) or
`Authorization: Bearer …` (OpenAI) header containing that local capability.
Missing, wrong, cross-run and duplicate capabilities receive 401 before any
body inspection, audit entry, budget charge or run signal. Authenticated policy
violations still produce `HijackAttempt`. The local capability is never the
provider key, is never forwarded upstream, and is excluded from relay debug
output and audit data. Wrapped launch diagnostics list environment variable
names without their values, including when nested inside another debug value.
Ordinary local probes cannot spend the run's provider
budget or be misattributed to its agent. This does not stop a same-user debugger
from inspecting process environments, a colluding host process from obtaining a
capability, or a local connection flood from occupying listener capacity. Such
host-process isolation needs a stronger operating-system boundary.

The relay accepts only origin-form HTTP/1.1 POST requests to:

- Anthropic: `/v1/messages`, `/v1/messages/count_tokens`.
- OpenAI: `/v1/responses`.

It fixes the upstream HTTPS origin, uses rustls, ignores host proxy settings,
and never follows redirects. `RelaySpec.test_upstream` accepts only loopback
fake servers; production leaves it unset.

Before local authentication, a Mac relay rejects requests with 401 as described
above. After authentication (or on Linux's private Unix socket), requests with
other methods/paths, queries, absolute targets, CONNECT, upgrades, encoded bodies,
`Expect`, invalid/duplicate-key JSON or bodies over 8 MiB receive 403 and a
hijack signal. Hosted tools, remote URL
inputs and background responses are also refused: otherwise the provider
could contact third parties or finish work outside the accounting window.
Local function/custom tools and inline data images remain usable. Requests
violating configured model or token policy are refused unchanged. When an
output ceiling is configured, generation requests must explicitly supply a
positive `max_tokens` or `max_output_tokens` within it; token-count requests
need no output limit.

Only `content-type`, `accept`, `anthropic-version`, `anthropic-beta`,
`user-agent`, `x-request-id` and `x-client-request-id` pass upstream; a
`Connection`-nominated header is removed too. Inbound authentication, cookies,
organization/project identity, forwarding headers and other hop headers never
pass. The relay injects its configured `x-api-key` or bearer key. Response
headers are limited to content type, request ids and retry timing.

Response chunks pass through immediately, including SSE. A bounded side
observer reads JSON usage and Anthropic `message_start`/`message_delta` or
OpenAI `response.completed` events; it never changes the response. The
observer holds at most 8 MiB for a JSON response or 1 MiB for one SSE event.
Missing/incomplete usage, observer overflow, interrupted upstream responses
and audit-write failures close admission and signal the budget. Accounting
continues after a client disconnect to detect overruns; a disconnected response
keeps its reservation. Requests are admitted serially, so queued requests can
use refunds from the preceding response. Spent budgets return 429, and
deadlines also terminate in-flight requests. Each request has a ten-minute
maximum lifetime and a ten-second request-body timeout.

Before forwarding, admission atomically charges the request count and reserves
one input token per byte of the larger of the original and forwarded JSON
bodies. Counting the entire body, including JSON overhead, is intentionally
conservative without a model-specific tokenizer and can refuse requests whose
actual input would fit. Generation limits are clamped to the remaining output
budget; without an explicit limit or configured policy ceiling, the relay
inserts the remaining allowance. No remaining output allowance or an input
bound that cannot fit returns 429 with the budget signal before any upstream
request. Token-count requests reserve no output tokens.

Complete JSON responses and SSE streams settle to provider-reported usage,
including cache tokens, and release unused input/output reservations only at
response end. Missing usage, dropped streams and cancelled requests retain the
full reservation. If a provider reports more than reserved, the relay records
the real usage and closes admission even if the stage budget is larger. The
provider must honor the forwarded output limit; unexpected provider overruns
cannot be recovered after spending.

Each authenticated HTTP request (every HTTP request on Linux's private Unix
socket) produces one JSONL entry with time, normalized
method/path, model, status, body byte counts, token counts, duration and
verdict. Unknown methods and blocked paths use fixed placeholders. Entries
contain no body text or header values. Invalid HTTP framing rejected by Hyper
before a request exists uses Hyper's protocol error response rather than the
request-policy audit path. Audit files are append-only from the relay,
created with mode 0600 and never mounted in the child.

### CLI settings verified

Verified locally with Codex 0.161.0 and Claude Code 2.1.284. No provider model
request or subscription usage was needed for this verification.

Claude's harness uses `ANTHROPIC_BASE_URL` with the run's relay port (18080 on
Linux), `ANTHROPIC_API_KEY` with the local capability on Mac or
`ophio-relay-not-a-key` on Linux, `--bare`, empty `--setting-sources`,
`CLAUDE_CODE_SIMPLE=1`, `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1`,
`DISABLE_TELEMETRY=1`, `DISABLE_ERROR_REPORTING=1` and `DISABLE_AUTOUPDATER=1`.
The installed `--help` confirms bare mode and its exclusion of OAuth/keychain
reads; all named environment controls are present in the installed native
binary. Their meanings were checked against the official
[environment reference](https://code.claude.com/docs/en/env-vars).

Codex's harness uses `-c` overrides for `model_provider="ophio_relay"`,
`forced_login_method="api"`, and `model_providers.ophio_relay` fields `name`,
`base_url="http://127.0.0.1:18080/v1"`, `wire_api="responses"`,
`env_key="OPHIO_RELAY_DUMMY_KEY"`, `requires_openai_auth=false` and
`supports_websockets=false`, with `analytics.enabled=false`. The named
relay-key environment variable holds the local capability on Mac or
`ophio-relay-not-a-key` on Linux. The base URL uses the per-run port on Mac.
Thread start/resume
selection and provider confirmation use the same provider id; relay runs do
not require a ChatGPT account. The installed app-server's `--strict-config`
accepted these keys and its `config/read` response returned the exact configured
values, using a temporary empty HOME. The official
[configuration reference](https://developers.openai.com/codex/config-reference/)
confirms the provider keys and HTTP Responses transport.

### Relay verification

Set `CARGO_TARGET_DIR` to an absolute scratch build directory and
`CARGO_BUILD_JOBS=2`. These focused selections use authored fake agents,
temporary dummy credentials and local fake upstreams, not installed agents or
provider inference. Run Mac commands one at a time on an authorized test host.
Do not replace these filters with the full Mac sandbox suite: existing account
and keychain tests have a different safety boundary.

On Linux or Mac:

```sh
cargo test --locked -p sandbox --lib relay::tests:: -- --test-threads=1
cargo test --locked -p harness-claude -p harness-codex --lib tests::sandbox::relay_ -- --test-threads=1
cargo test --locked -p ports --lib sandbox::tests::wrapped_debug_keeps_environment_names_and_never_values -- --exact --test-threads=1
```

Relay coverage includes authentication replacement, unrelated-probe isolation,
request refusal/signals/audits, SSE delivery before upstream completion,
fragmented event usage, JSON usage, all budgets, concurrent admission,
disconnect accounting, model/token policy and shutdown. Linux HTTP unit clients
use the relay's private Unix socket; separate TCP-forwarding coverage remains:

```sh
cargo test --locked -p sandbox --lib shim::tests::forwards_both_directions_and_half_close -- --exact --test-threads=1
cargo test --locked -p sandbox --lib linux::tests::relay_ -- --test-threads=1
```

On Mac, these additional fixtures exercise real Seatbelt profiles with authored
shell/Python programs and temporary files. They disable the fixture's keychain
lookup, do not sign in or renew credentials, and use a zero-budget relay for
network refusal checks. The containment test deliberately proves that a
detached descendant survives process-group cleanup, then ends it through its
private input pipe; it does not claim complete containment.

```sh
cargo test --locked -p sandbox --lib macos::relay_tests:: -- --test-threads=1
cargo test --locked -p containment --test macos_contract -- --test-threads=1
```

These selections do not authorize live agents, keychain changes, paid model
calls, desktop interaction, host provisioning or service changes. Compilation
alone is not evidence that the native profile enforces the contract.

The Linux-only opt-in test below additionally checks that the relay answers
while provider addresses, an unrelated external address and a host loopback
listener are unreachable, and credential/log paths are absent. Its budget is
zero, so it makes no upstream provider request. It requires a host that permits
bubblewrap; do not remove isolation or change service settings to bypass a
refused namespace probe.

```sh
cargo test --locked -p sandbox --test live relay_is_the_only_network_route_and_credentials_do_not_exist -- --ignored --nocapture
```
