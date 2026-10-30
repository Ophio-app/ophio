# Claude Code fixture qualification

Inspected on **2026-09-27**: `claude --version` reported **2.1.280 (Claude Code)**.
Only version/help discovery ran against the installed CLI. No native prompt,
inference, credential-file access, or user configuration edits were performed.
The fake CLI in `tests/fake_claude.py` is authored test data, not a user transcript.

Evidence:

- Installed `claude --help`, `claude auth --help`, and `claude auth status --help`.
- [Official CLI reference](https://code.claude.com/docs/en/cli-reference): print,
  stream-json, partial messages, replay-user-messages, model, effort, resume,
  MCP configuration, permission modes, and auth status.
- [Official streaming input guide](https://code.claude.com/docs/en/agent-sdk/streaming-vs-single-mode): user text and image content blocks.
- [Official SDK transport source](https://github.com/anthropics/claude-agent-sdk-python/blob/main/src/claude_agent_sdk/_internal/query.py): control_request interrupt and correlated control_response envelopes. This is moving source, not a pinned CLI wire schema.

The installed help establishes the flags. Stream envelopes, tool blocks, usage,
and native interrupt framing are derived from the documented interfaces; they
have **not** been recorded from 2.1.280 running inference. Version-to-wire
compatibility, native subscription readiness JSON fields, permission-tool
argument variations, and live turn/child ordering still require qualification.
Models are not guessed. The actual model is taken from native initialization;
actual effort remains unknown unless reported.

The model catalog is the list Claude Code gives its own picker: on
**2026-10-09**, against the installed **2.1.295**, an `initialize` control
request on stdin (`-p` with stream-json, user settings only, hooks off, no MCP
servers, no saved session) answered with `models` entries carrying `value`,
`resolvedModel`, `displayName`, `description`, `supportsEffort` and
`supportedEffortLevels`, and no prompt or turn was sent. The fake answers the
same request with a list shaped like that one. A CLI that doesn't answer falls
back to the choices its `--help` documents.

## Integration

Construct `ClaudeAdapter`, pass the broker to
`computer_mcp::serve::serve_bridge`, and start the run with the adapter. Register a `ToolBridge` with server name `computer` and
permission tool `permission_prompt`. The adapter creates a private temporary
MCP JSON file and passes `--permission-prompt-tool
mcp__computer__permission_prompt`, and the same name to `--disallowedTools`:
Claude Code resolves the prompt tool from every MCP tool and calls it itself,
while the deny rule keeps the model from calling it. The file lives until
process exit and contains no bridge token. Only the two bridge environment constants are accepted as
process environment overrides; their values are absent from argv and JSON.
The supplied bridge command/args must themselves contain no secrets.

The native permission tool calls the MCP process. The MCP process forwards the
original tool name/input, optional native tool-use id, and its enclosing native
MCP JSON-RPC request id to the authenticated broker connection. Request ids are
JSON-encoded (`7` and `"7"` remain distinct). The adapter emits no approval
events of its own: the broker alone records the approval and owns the user
decision, scope checks, cancellation, and the exact Claude decision:
`{"behavior":"allow","updatedInput":...}` or
`{"behavior":"deny","message":...}`. The MCP process returns that decision as
JSON text in its tool result. `RunControl::resolve_approval` is deliberately
unsupported for this harness: the decision completes the broker's outstanding
`handle` future. On disconnect the broker must cancel outstanding
approvals in `unbound`; a cancelled handle future does not fabricate a decision.
Request identities are scoped to one bound connection/run; the broker must not
reuse a consumed run token for concurrent or replacement bridge processes.

## Delivery and lifecycle

- Streaming writes return `HostAccepted`. A correlated native user-message echo
  upgrades delivery to `NativeDelivered`; neither claims the model acted on it.
- `NextTurn` stays in a bounded adapter queue until a native result boundary.
  `InterruptAndSend` requests a native interrupt, then waits for that boundary.
- Native interrupt has an independent control queue and correlated response.
  A stalled pipe write fails after two seconds; shutdown closes stdin, waits
  three seconds, and then kills the process (and its owned Unix process group).
- The event queue is bounded. If its receiver stalls, the run is stopped with
  a failed exit instead of silently losing approval/lifecycle events.
- Images are read once, size bounded, and encoded as base64 content blocks.
- A native Task tool invocation is visible as a child; a tool result alone does
  not prove a background child's completion. Native task notifications provide
  stronger completion evidence.

Auth preflight uses the CLI's documented status command, never credential-file
contents. API-key/provider environment overrides are removed, and only a
reported native Claude subscription login is accepted. This does not certify
arbitrary user wrappers, managed settings, hooks, or alternate desktop drivers.
The broker must revoke/fence desktop authority before native Stop. Native
interrupt is not proof that detached children or arbitrary shell processes ended.
Windows process-tree cleanup and native Windows interaction remain unverified.

Tests cover streaming/model identity, activity/children, usage, unknown lines,
resume/model/effort/MCP flags, token-free argv/config, image blocks, native
interrupt bytes, queued and streaming follow-ups, nonzero exit, and the real
permission relay. MCP binary round trips are tested in `computer-mcp`.

The Linux containment turn gate reads all five `ready` bytes before writing
the release byte and closing its socket. The regression
`turn_gate_waits_for_all_readiness_bytes` holds the peer before readiness and
after a partial message, then verifies release once the message is complete.
It exercises the ordering without sleeps or a native Claude process; the
containment tests still require a completed run and verify child cleanup.

Crate dependencies: base64 0.22.1 (MIT OR Apache-2.0), tempfile 3.23.0
(MIT OR Apache-2.0), uuid 1.26 (MIT OR Apache-2.0), and Unix rustix 1.1.2
(Apache-2.0 OR Apache-2.0 WITH LLVM-exception OR MIT). Cargo.lock pins the
resolved compatible versions.

## Task modes

The 2.1.280 `claude --help` probe exposes `--permission-mode` choices including
`manual` and `plan`, plus `--tools`, `--safe-mode`, `--strict-mcp-config`,
`--mcp-config`, `--permission-prompts`, `--append-system-prompt`,
`--system-prompt-snapshot` and `--disallowedTools`. Probe and restricted dispatch both
check help; a missing plan/isolation flag makes Ask and Plan unsupported with
a reason. No model request is used for discovery.

Ask and Plan use the same restriction: `--permission-mode plan --tools
Read,Glob,Grep --safe-mode --strict-mcp-config --mcp-config <empty-server-file>
--permission-prompts none`. The mode alone is not treated as a shell sandbox.
Only the three read tools are exposed; customizations and other MCP servers are
disabled, no permission bridge credentials are passed, and requests that would
need permission cannot escalate the run. Resume passes the same restrictions.
Work retains `--permission-mode manual` and the existing permission-prompt MCP
bridge. A computer-use bridge cannot be supplied to a restricted run.

Capability enforcement text:

| Mode | Available when | Enforcement |
| --- | --- | --- |
| Ask | All required help flags are present | Claude Code plan mode; only Read, Glob and Grep; custom tools and permission escalation disabled |
| Plan | All required help flags are present | Claude Code plan mode; only Read, Glob and Grep; custom tools and permission escalation disabled |
| Work | Native executable is installed | Claude Code manual permissions; prompts relayed to the app |

Authored-fixture tests:

- `modes_probe_and_launch_enforce_read_tools_on_start_and_resume`
- `missing_plan_flag_is_unsupported_and_never_launches_inference`

The permission relay emits an unknown closure outcome: it forwards a broker
answer, not a native terminal decision. The existing relay test explicitly checks
`outcome: None`. Runtime qualification with a real subscription remains separate
from the help/schema and fake-process checks performed here.

## Intent framing

Discovery against Claude Code 2.1.280 confirmed that `--append-system-prompt`
accepts one prompt value. Mission text and the shared mode instruction are joined
with a blank line into that one value. Work's arguments and mission text are
unchanged. These instructions express intent; native tool restrictions remain
responsible for enforcement.

- Ask: “Answer the person's question using the project. Change nothing. Do not produce a change plan unless asked. Reply directly with your answer.”
- Plan: “Investigate the project and reply with a concrete, reviewable step-by-step plan for a later Work task. Change nothing. Include the plan in your reply text.”

Ask additionally passes `--disallowedTools ExitPlanMode`, whose deny-list syntax
is advertised by help. Both read-only modes pass `--system-prompt-snapshot off`:
help says the default snapshot can otherwise reuse an earlier system prompt
on resume, ignoring a later append. Follow-ups use the same process policy;
resumed runs reconstruct the same instruction and flags.

An assistant `ExitPlanMode` tool block containing `input.plan` is also translated
to an `AgentMessage` in Plan mode, so its plan appears as timeline prose.
The authored fake stream exercises this shape followed by a permission callback;
the callback is rejected, and no writing grant is issued. Ordinary assistant text
continues through the existing reply parser. No live model was invoked.

Tests compare the complete normalized argv for every mode on start and resume,
exercise follow-ups, and check that missions use exactly one append flag.

## Thinking progress

Claude Code 2.1.284 writes `{"type":"system","subtype":"thinking_tokens",
"estimated_tokens":…,"estimated_tokens_delta":…,"user_message_uuid":…}` while
the model thinks. The shape is taken from the stream-json schema bundled in
that executable; the values in the test are authored. The adapter reads these
lines and shows nothing for them: the app has no thinking progress to update,
and the turn's usage counts the tokens once the turn ends.

## Exact model selection

Catalog discovery runs without tools, hooks, MCP servers or session persistence
in an isolated directory. An authoritative empty native list stays empty. The
app keeps descriptive alias labels and also offers each native resolved exact ID.
An explicit `claude-...` selection is checked in the task process before sending
any prompt, including on resume. Its native list and applied settings must agree;
timeouts and missing metadata fail the run. Per-process fallback controls prevent
model substitution, and later main-session model changes fail immediately.
Synthetic API refusals retain their native reason. No global settings are changed.

`src/tests/models.rs` covers these paths with synthetic processes, including Stop
while verification is pending. `tests/inspect_native_model.py` is a manual,
metadata-only qualification tool; it is not run by the suite. The original #29
qualification applies to its recorded native executable, not automatically to
other installed versions. Native inference and Windows runtime qualification
remain separate from these fixture checks.
