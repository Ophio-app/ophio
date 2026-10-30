# Linux conversation sandbox

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
transcripts are not mounted. Only a requested native session is imported, and
only the reported session's transcript is exported after containment ends, so
later unrestricted turns can resume it. Host credential contents are never
inspected or copied by the sandbox.

Restricted conversations do not receive the companion's host tool bridge.
Provider-hosted search and cloud connectors are disabled: those would execute
outside the host namespace. Local shell network tools use the live proxy policy.
Blocks reported by the proxy and absolute denied paths in native tool results
become `Task.access_blocked` with a project match and `TaskUpserted`. Tool-output
recognition is best effort, not a kernel audit stream.

## Verification

`cargo test -p sandbox` covers policy, history isolation and mount construction.
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
client change. Windows and macOS currently report unsupported.

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

This mode replaces the CONNECT proxy. The same shim forwards namespace
`127.0.0.1:18080` to the relay's Unix socket. There is no other network route,
even when internet or local-network grants are set or later changed. A relay
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
provider-state location and dummy provider authentication enter the child.
Runs without a relay retain their existing subscription, proxy and history
behavior.

The relay accepts only origin-form HTTP/1.1 POST requests to:

- Anthropic: `/v1/messages`, `/v1/messages/count_tokens`.
- OpenAI: `/v1/responses`.

It fixes the upstream HTTPS origin, uses rustls, ignores host proxy settings,
and never follows redirects. The plain HTTP upstream override exists only in
unit tests. Requests with other methods/paths, queries, absolute targets,
CONNECT, upgrades, encoded bodies, `Expect`, invalid/duplicate-key JSON or
bodies over 8 MiB receive 403 and a hijack signal. Hosted tools, remote URL
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

Each accepted HTTP request produces one JSONL entry with time, normalized
method/path, model, status, body byte counts, token counts, duration and
verdict. Unknown methods and blocked paths use fixed placeholders. Entries
contain no body text or header values. Invalid HTTP framing rejected by Hyper
before a request exists uses Hyper's protocol error response rather than the
request-policy audit path. Audit files are append-only from the relay,
created with mode 0600 and never mounted in the child.

### CLI settings verified

Verified locally with Codex 0.161.0 and Claude Code 2.1.284. No provider model
request or subscription usage was needed for this verification.

Claude's harness uses `ANTHROPIC_BASE_URL=http://127.0.0.1:18080`,
`ANTHROPIC_API_KEY=ophio-relay-not-a-key`, `--bare`, empty `--setting-sources`,
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
`supports_websockets=false`, with `analytics.enabled=false`. The named dummy
environment variable holds `ophio-relay-not-a-key`. Thread start/resume
selection and provider confirmation use the same provider id; relay runs do
not require a ChatGPT account. The installed app-server's `--strict-config`
accepted these keys and its `config/read` response returned the exact configured
values, using a temporary empty HOME. The official
[configuration reference](https://developers.openai.com/codex/config-reference/)
confirms the provider keys and HTTP Responses transport.

### Relay verification

`env -u WAYLAND_DISPLAY -u DISPLAY cargo test -p sandbox -p ports -p harness-claude -p harness-codex -p services`
uses localhost fake upstreams and Unix sockets. Relay coverage includes auth
replacement, request refusal/signals/audits, SSE delivery before upstream
completion, fragmented event usage, JSON usage, all budgets, concurrent
admission, disconnect accounting, model/token policy and mount secrecy.

`env -u WAYLAND_DISPLAY -u DISPLAY cargo test -p sandbox --test live relay_is_the_only_network_route_and_credentials_do_not_exist -- --ignored --nocapture`
additionally checks that the relay answers while provider addresses, an
unrelated external address and a host loopback listener are unreachable, and
credential/log paths are absent. Its budget is zero, so it makes no upstream
provider request. The namespace probe in this build environment refused user
namespace creation; this live check needs a host that permits bubblewrap.
