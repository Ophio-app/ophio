# Run-bound computer tools

The `computer-mcp` executable is a stdio MCP facade. The companion runs
`serve::serve_bridge(path, Arc<dyn BridgeHandler>, shutdown_future)` and gives
the native harness a `ToolBridge` carrying `OPHIO_BRIDGE_SOCKET` and
`OPHIO_BRIDGE_TOKEN` in its environment. The executable and MCP server name
are defined by `BINARY_NAME` (`[[bin]].name` is Cargo's matching build metadata).
Permission tool identity is `PERMISSION_TOOL`.

The broker authenticates Hello, owns the run/target/lease binding, and supplies
Welcome. No model argument can select a run, target, token, or lease. Welcome's
`computer_use`, `browser` and `github` flags decide which tools the run gets, as
the table below shows. A call to a tool the run wasn't given is refused before
it reaches the socket. The broker check in `serve.rs` refuses most of them
again: computer actions from a run without computer use, `request_human` from a
run without computer use or the browser, browser and GitHub requests without
their flag, and person and work requests from the computer specialist. The
executable also refuses `permission_prompt` from the computer specialist, and
the work tools other than `ophio_agents` and `ask_agent` from a run linked to
GitHub. The service's work handler enforces that GitHub restriction
again using the persisted task: the bridge lets agent lookup and read-only
helper requests reach it, while still rejecting all work requests from computer
specialists. Bridge-level service tests cover issue and pull-request tasks,
forbidden planning calls, and stopped runs. In Through merge, pending helper
consultations and undelivered answers hold automatic merging. Delivery resumes
the caller, whose active turn keeps the merge line waiting while it handles the
answer; ready and merge requests recheck the same pending-answer hold.
If an answer expires or cannot be delivered, the open pull request is handed
to the person to check. Dropping the answer never authorizes automatic merging.
Read-only helpers receive no tool bridge or GitHub posting tools.

The listener requires an owner-only directory on Unix, creates the socket with
mode 0600, refuses to replace existing paths, and removes its socket on shutdown.
Windows uses a local named pipe with a protected owner/System DACL, remote
clients rejected, and first-instance checking. Binding has a five-second
handshake deadline. Each connection has one binding; disconnect/shutdown calls
`unbound`. The handler must implement cancellation-safe handle/bind futures and
cancel pending approval/human-step work on unbound. It must also reject replayed
or already-consumed run tokens. Accepting another connection is not a reconnect
authorization. Listener shutdown drains unbound callbacks; the broker must make
those callbacks finite.

Tool names are the exact names `tools/list` returns, and the names a client or
tool allowlist (Claude Code's `mcp__<server>__<tool>`, say) must use. The lists
are constants in `ports::bridge`.

| Run | Tools listed |
|---|---|
| Computer specialist (`computer_use`) | `observe_target`, `pointer_action`, `key_action`, `type_text`, `scroll` and `request_human` (`COMPUTER_TOOLS`). Nothing else, not even `permission_prompt`, and the `browser` and `github` flags change nothing. |
| Any other run | `permission_prompt`; `show_person`, `show_preview`, `invite_person`, `wake_me`, `can_ask`, `checklist_read`, `checklist_add`, `checklist_remove` and `checklist_reply` (`PERSON_TOOLS`); `ophio_agents`, `ask_agent`, `github_new_issue`, `github_start_issue` and `github_start_review` (`WORK_TOOLS`). |
| … with `browser` | Adds `browser_open`, `browser_back`, `browser_tabs`, `browser_switch_tab`, `browser_close_tab`, `browser_observe`, `browser_click`, `browser_type`, `browser_press`, `browser_scroll`, `list_logins`, `fill_login`, `fill_code` and `ask_owner_for_code` (`BROWSER_TOOLS`), and `request_human`. |
| … with `github` | Adds `github_pr`, `github_comment`, `github_issue`, `github_ready`, `github_merge` and `github_cannot_reproduce` (`GITHUB_TOOLS`), and keeps only `ophio_agents` and `ask_agent` (`ASK_TOOLS`) of the work tools. |

`tools/call` still accepts the older names `observe`, `pointer` and `key` for
`observe_target`, `pointer_action` and `key_action`, but they are never listed.

`pointer_action` and `scroll` coordinates are image pixels, converted using the
last observation's dimensions. The geometry revision is forwarded unchanged;
both local stale-image checks and broker stale-geometry checks can refuse input.
`key_action` and `type_text` authority remains the broker's responsibility, as
those bridge requests have no geometry fields. The broker must enforce its
lease/target and observation policy for every operation, not just
coordinate-bearing calls.

Refusals and broker errors become visible MCP tool errors. A lost socket or
transport timeout permanently disables the connection; uncertain actions are
never replayed. Frames are bounded at 32 MiB, calls are serial, and a call timeout
is one hour to accommodate human permission/handoff steps. This is a transport
limit, not a desktop lease lifetime. The broker may and should return earlier
on revocation, expiration, or shutdown. Tokens and content are never logged.

Protocol support follows the [MCP 2026-07-28 versioning specification](https://modelcontextprotocol.io/specification/2026-07-28/basic/versioning):
modern requests carry version and client capabilities in `_meta`, and
`server/discover` exposes supported versions. The legacy `initialize` /
`notifications/initialized` path negotiates 2025-11-25, 2025-06-18, 2025-03-26,
and 2024-11-05. Optional resources, prompts, sampling, tasks, and subscriptions
are not advertised.

`tests/stdio.rs` spawns the built binary and exercises real Unix socket round
trips, both capability sets, legacy/modern negotiation, native permission ids,
observation content, stale-coordinate refusal, invalid-token exit, and socket
loss during a pending call. A unit test in `src/mcp.rs` checks that this README
names every tool `tools/list` can return. Windows is cross-compiled; its ACL and
pipe behavior still require native Windows qualification. No real desktop or
inference is used by tests.

Dependencies: Unix rustix 1.1.2 (Apache-2.0 OR Apache-2.0 WITH LLVM-exception
OR MIT), Windows interprocess 2.4.4 (0BSD OR Apache-2.0) and widestring 1.2.1
(MIT OR Apache-2.0), and test-only tempfile
3.23.0 (MIT OR Apache-2.0). Resolved versions are pinned by Cargo.lock.
