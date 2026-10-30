# Codex app-server fixture qualification

Inspected on **2026-09-27**: `codex --version` reported **codex-cli 0.153.2**.
Discovery used only `--version`, `app-server --help`, `login --help`, and
`app-server generate-json-schema --out ...` / `generate-ts --out ...`. Generated
inspection files stayed in the ignored build directory. No installed app-server
was started, no `turn/start` was sent to a real tool, and no inference, account
login, credential-file reads, or user config edits were performed.

The [official app-server documentation](https://learn.chatgpt.com/docs/app-server)
and the **installed 0.153.2 generated v2 schema** define the implemented subset.
`wire.rs` hand-writes only the required envelope/input/thread/turn/usage types;
unknown fields are tolerated and unknown notifications produce warnings.
`tests/fake_codex.py` is authored fixture data, not a captured user session.

Schema-checked surfaces:

- initialize/initialized; account/read; thread/start and thread/resume;
- turn/start (text and image data URL), turn/steer with expectedTurnId, and
  turn/interrupt with threadId/turnId;
- thread model/modelProvider/reasoningEffort responses, item/turn notifications,
  and thread/tokenUsage/updated;
- item/commandExecution/requestApproval and item/fileChange/requestApproval:
  `{"decision":"accept"}` or `{"decision":"decline"}`;
- item/tool/requestUserInput: `{"answers":{"question-id":{"answers":[...]}}}`;
- MCP form elicitation response fields and native permission grant scopes.

No API billing fallback is provided. App-server must report an account of type
`chatgpt` before the initial turn is dispatched. The process uses native sign-in,
`forced_login_method="chatgpt"`, and `model_provider="openai"`; API credential
and base-URL environment overrides are removed. Probe uses only `--version`, generated schema inspection,
and `login status`, does not print account command output, and reports unknown
when the status is not a recognized ChatGPT login. It does not guess a model
catalog or effort choices.

## Integration and exact decisions

Use `CodexAdapter::new()` or `with_binary(path)`. No run registers the companion
bridge: Codex asks for approval through app-server requests, never through the
bridge's permission tool, and computer use is refused, so a Work run's only
bridge tool would let the model raise approval cards that authorize nothing.
No file under the person's Codex configuration is edited.

`native_request_id` is the JSON encoding of the server request's original id:
number `71` and string `"71"` remain distinct. Keep it opaque and pass it back
unchanged to `resolve_approval`. Native responses retain that original id type.
Approvals are closed by native resolution, turn completion, or process exit;
duplicate, cancelled, and unknown approval ids cannot be reused. Arbitrary
server requests (including external token refresh requests) get a JSON-RPC error,
never credentials or an automatic approval.

A single question accepts `ApprovalDecision::Answer` text. Multiple questions
require an answer text containing a JSON object mapping each exact native
question id to an array of answer strings. One answer is never silently applied
to every question. MCP form answers accept a JSON object; URL elicitations may
be denied but cannot be falsely marked completed by this contract.

Native `item/permissions/requestApproval` grants last a **turn or session**.
The shared `ApprovalDecision::Allow` authorizes **one effect**, so this adapter
refuses Allow for that request and leaves it pending; Deny sends an empty
permission profile with turn scope. The integration needs a separately scoped
approval decision before broader grants can be offered. No session/persistent
allow or policy amendment is synthesized from Allow.

## Delivery and lifecycle

Steering is native-acknowledged and guarded by expectedTurnId. NextTurn is held
in a bounded queue until the current turn ends. InterruptAndSend interrupts
first and waits for completion. Request responses and notifications may
interleave; late start responses must not resurrect a completed turn. A stop
while the native turn id is in flight is latched until the id arrives. A stop
before dispatch prevents the initial turn from being submitted.

Shutdown closes stdin, waits three seconds, and then kills the process (and the
owned Unix process group). Native request acknowledgements time out; failed or
unknown external effects are never retried. A stalled event consumer stops the
run rather than silently dropping approval/lifecycle events. Images are frozen
into size-bounded data URLs before dispatch.

Live qualification is still required for installed-version inference/approval
round trips, schema changes, real account/plan eligibility, actual child-agent
ordering, resume/reconnect edge cases, and Windows runtime behavior. The broker
must fence desktop authority independently; native interrupt does not establish
that detached/remote children or every shell process ended. A generated schema
is not a production support commitment from the native tool vendor.

Crate dependencies: base64 0.22.1 and tempfile 3.23.0 (each MIT OR Apache-2.0),
and Unix rustix 1.1.2 (Apache-2.0 OR Apache-2.0 WITH LLVM-exception OR MIT).
Cargo.lock pins the resolved compatible versions.

## Task modes and closure outcomes

Rechecked against the installed **0.153.2** using `codex --version`,
`codex app-server --help`, and `codex app-server generate-json-schema --out
<ignored-build-directory>`. The inspected files were `v2/ThreadStartParams.json`,
`ThreadResumeParams.json`, `TurnStartParams.json`, `ThreadStartResponse.json`, and
`ServerRequestResolvedNotification.json`. Probe now generates the schema into a
private temporary directory and checks the exact sandbox/approval fields and
enums; failed or incompatible discovery makes restricted modes unsupported.
Restricted dispatch repeats that check. No real thread or turn was started.

Ask and Plan set `sandbox: "read-only"` on both `thread/start` and
`thread/resume`. Every `turn/start`, including queued follow-ups, sets
`sandboxPolicy: { type: "readOnly", networkAccess: false }`. Thread and turn
requests both set `approvalPolicy: "on-request"` and `approvalsReviewer: "user"`.
The thread response must confirm those restrictions before the first turn is
sent. A writable, automatic-review, missing or malformed response fails closed.
Work retains its existing parameters, including the user's native sandbox.
Its thread response must still report a `readOnly` or `workspaceWrite` sandbox,
`on-request` approvals and, when reported, the person as reviewer. Full access,
an external sandbox or automatic review fails closed before the first turn.

Write, command-escape, permission-grant and external-tool approvals cannot be
allowed during Ask/Plan; genuine agent questions may be answered and effects may
be denied. The service rejects prohibited decisions before claiming an approval,
and the adapter independently checks the decision. Changing authority requires a
new Work task. These rules apply unchanged on resume.

Per-process configuration replaces `mcp_servers`, `plugins` and `hooks` with empty
tables and `apps` with `{ _default: { enabled: false } }` for restricted runs.
External tools are not protected by the command sandbox, so the companion bridge
is also omitted. The [configuration reference](https://learn.chatgpt.com/docs/config-file/config-reference)
documents those tool/customization settings. Qualification of the actual native
sandbox and configuration behavior on supported Linux/Windows builds remains
necessary before shipping; the fixtures prove the parameters and refusal paths,
not operating-system enforcement.

| Mode | Available when | Enforcement |
| --- | --- | --- |
| Ask | Required schema fields/enums are confirmed | Codex read-only sandbox, network disabled; write and escape approvals refused |
| Plan | Required schema fields/enums are confirmed | Codex read-only sandbox, network disabled; write and escape approvals refused |
| Work | Native executable is installed | Codex native sandbox, confirmed on each thread; approval requests reviewed by the app |

`serverRequest/resolved` has only `threadId` and `requestId` in the inspected
schema, matching the [app-server documentation](https://learn.chatgpt.com/docs/app-server).
Resolution, turn cleanup and process exit therefore emit `outcome: None`. No
outcome is guessed from activity or run continuation. A notification after our
own response is not misclassified as an external decision.

Authored-fixture tests:

- `modes_keep_read_only_sandbox_and_user_approvals_on_start_resume_and_followup`
- `missing_read_only_schema_is_unsupported_and_never_launches_inference`
- `native_read_only_acknowledgement_is_required`
- `unconfirmed_read_only_thread_never_receives_a_turn`
- `read_only_decisions_never_authorize_effects_or_broader_permissions`
- `server_resolution_without_a_decision_keeps_outcome_unknown`

`tempfile` 3.23.0 (MIT OR Apache-2.0) is a runtime dependency for schema
inspection as well as a test dependency.

## Shared contract regression coverage

The additive fields retain older command, stored-row and TypeScript callers.
Task mode is fixed for the task's lifetime. Resume text and markup are recorded
in the admission transaction and handed to the adapter through the same prompt
conversion as task creation. The existing `HostAccepted` delivery state is used:
the timeline's run ID identifies which run the message starts, without claiming
native delivery before dispatch. Tests cover:

- `task_mode_and_resume_bundle_are_optional_on_the_wire`
- `legacy_task_and_approval_rows_default_without_escalation_or_invented_outcomes`
- `create_rejects_unavailable_mode_with_reason_and_non_work_computer_use`
- `legacy_modes_are_work_only_and_resume_keeps_the_persisted_mode`
- `resume_markup_is_admitted_once_delivered_and_replayed_without_duplication`
- `text_only_and_markup_only_resume_send_exactly_the_new_message`
- `admitted_resume_before_dispatch_recovers_with_one_message_and_no_relaunch`
- `resume_reuses_bundle_validation_and_revision_guards_without_appending`
- `annotation_ownership_is_checked_for_create_send_and_resume`
- `read_only_write_approval_is_rejected_before_claim_and_can_still_be_denied`
- `native_approval_closure_persists_and_emits_only_reported_outcomes`
- `resolved_elsewhere_displays_reported_outcome_or_honest_unknown`

## Intent framing

The installed 0.153.2 JSON schema confirms `developerInstructions` with type
`["string", "null"]` on both `ThreadStartParams` and `ThreadResumeParams`.
The adapter checks both fields during read-only qualification, combines the
mission (when present) with the shared mode instruction, and sends it on
`thread/start` or `thread/resume`. Subsequent turns retain that thread context.
Work's argv, thread/turn parameters and serialized mission remain unchanged.

- Ask: “Answer the person's question using the project. Change nothing. Do not produce a change plan unless asked. Reply directly with your answer.”
- Plan: “Investigate the project and reply with a concrete, reviewable step-by-step plan for a later Work task. Change nothing. Include the plan in your reply text.”

If a compatible sandbox schema lacks a confirmed string instruction field on
either thread method, the adapter uses a `<task-mode-intent>` preamble on the
first text block of every submitted input, including resumed, steered and queued
follow-up inputs. It does not send an unconfirmed instruction field. This fallback
is covered by fake binaries; the installed schema uses developer instructions.
Framing never replaces the sandbox or approval policy.

Tests compare complete argv and thread/turn params for each mode, start/resume
and follow-up; additional tests cover mission composition and the fallback.

## Warnings and the code-mode helper

On 2026-09-29, codex-cli 0.153.2 and 0.157.1 were driven through a thread and
one turn against a local scripted Responses endpoint, with a throwaway
`CODEX_HOME` and no account or inference. Both list `code_mode_host` as an
enabled feature, and the default models run every command through Codex's
`exec` tool, which starts `codex-code-mode-host` from beside the Codex
executable. The 0.153.2 copy tried had no helper beside it: at the thread's
start the app-server sent `{"method":"warning","params":{"threadId":…,
"message":"Code Mode is unavailable because failed to spawn code-mode host
…/codex-code-mode-host: host executable was not found. Code mode will fail
closed; …"}}`, and each command the model asked for failed with "failed to
spawn code-mode host". The 0.157.1 installation, with the helper beside it,
ran the same command. The warning test replays that notification with its
path replaced.

Readiness reports a native Codex executable without the helper beside it as
unable to start tasks, and a Codex older than 0.153.2, the oldest version this
adapter is qualified with (`readiness::MINIMUM`), as incompatible. A warning
reaches the task in plain words: the helper's with what to do, others as
"Codex warns: …".

The first live run with 0.157.1 showed two more notifications on a resumed
thread. `deprecationNotice` said full-history hydration is deprecated for
paginated threads and to pass `excludeTurns: true`; the adapter never reads
the earlier turns, so `thread/resume` now passes it, which 0.153.2 accepts
too, and the notice stopped, as a second scripted-endpoint resume on both
versions showed. Any other deprecation notice reaches the task as "Codex
notes: …". `thread/goal/cleared` and `thread/goal/updated` report Codex's
thread goals, which nothing in the app follows, so they are read and not shown.
