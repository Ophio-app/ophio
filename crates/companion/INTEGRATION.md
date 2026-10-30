# Linux composition verification

The Linux executable composes the real adapters behind `CompanionHub`. Tests
use authored native CLI fixtures and a synthetic Linux desktop driver; they do
not use inference, an actual desktop session, account files or external network
services.
Voice tests use an injected fixture factory and no model download. Telegram uses
a loopback Bot API; linking and task admission use the real Hub and paired TLS phone.

## Running them

```sh
cargo build -p companion -p computer-mcp
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
env -u WAYLAND_DISPLAY -u DISPLAY -u DBUS_SESSION_BUS_ADDRESS SHELL=/bin/sh cargo test --workspace
```

The fixture terminal uses `/bin/sh`; the CLI subprocess additionally clears its
environment and uses private temporary HOME/XDG directories and authored
discovery/harness commands.

With the LLVM/MinGW toolchain from `BUILDING.md`:

```sh
CARGO_TARGET_DIR="$PWD/target/windows-check" cargo check -p companion --target x86_64-pc-windows-gnullvm
```

This is a compile check, not Windows runtime qualification.

## Regression coverage

| Test | Evidence |
| --- | --- |
| `real_composition_tls_claude_allow_deny_codex_terminal_library_transfer` | Persisted identity, TLS pairing and channel authentication; native Claude MCP allow/deny; Codex start/stop; PTY echo/close; live roots, read/save/conflict/revocation; collision upload and download; loopback preview; identity retained after restart |
| `owner_desktop_toggle_applies_live_and_private_roots_are_rejected` | LinuxDesktop with a synthetic native driver, real tile encoding and TLS media; lease acquisition; disable ends input and stream with a reason; private roots rejected; missing bridge prevents Claude starts |
| `shutdown_drains_active_run_terminal_transfer_and_releases_sockets_and_locks` | Shutdown during a pending approval, live PTY and partial transfer; harness and shell PIDs gone; sockets removed; locks reacquired; transfer paused on restart |
| `binary_run_status_live_config_and_offline_config_use_isolated_directories` | Actual run/status/doctor/roots/retention/desktop subprocesses, headless remedy, SIGTERM shutdown and stopped-service config editing |
| `supplied_host_identity_survives_restart_and_mismatch_fails_closed` | Stable host id and explicit rejection of mismatched identity/store |
| `desktop_removal_revokes_lease_and_ends_stream_with_reason` | Input revocation, observation fencing, stream end and intervention receipt |
| `retention_change_compacts_the_running_store` | Live retention produces an explicit journal gap |
| `bridge_connection_is_single_use_and_disconnect_expires_pending_approval` | No token reuse; pending broker approval and waiter cancelled on disconnect |
| `transient_accept_errors_keep_listener_alive` | Injected EMFILE, ENFILE, ECONNABORTED and ECONNRESET; no fd-limit manipulation |
| `fatal_accept_error_and_cancelled_backoff_terminate` | Fatal listener failure and cancellation during bounded backoff |
| `collision_suffix_precedes_extensions` | Ordinary extensions, compressed tar extensions, dotfiles and extensionless names |
| `allow_is_one_effect_and_never_turn_or_session_permission` | Codex Allow maps only to one-time `accept`; broader permissions cannot receive Allow |

The existing crash-window regression
`admission_deduplicates_and_recovers_intent_before_dispatch` also verifies that
shutdown never starts work which was admitted but had not begun dispatch.

## Authored end-to-end run

Over the authenticated control socket, an isolated run shows `CommandProgress`,
`RunUpserted`, `ApprovalUpserted` and `TimelineAppended`. For both Allow and
Deny, each Claude task produces exactly one approval, resolved at revision 1, a
persisted decision, an agent continuation, five timeline items and `RunEnded`
with Completed state.

The fixture checks the exact MCP decision returned to the native process.
Native stop produces an `InterventionRecorded` receipt for Codex. Live root
changes emit `LibraryRootsChanged`; the upload preserves the old file and
publishes `notes (1).md`.

Excerpt from `ophio status --json` in a temporary XDG directory tree:

```json
{
  "voice": {
    "model": "tiny.en",
    "availability": {
      "state": "permission_missing",
      "remedy": "ophio voice install tiny.en"
    },
    "remedy": "ophio voice install tiny.en"
  },
  "channels": [
    {"channel": "telegram", "state": "disabled", "remedy": "ophio channels enable telegram"},
    {"channel": "discord", "state": "disabled", "remedy": "ophio channels enable discord"}
  ]
}
```

Separate assertions compare the hub id with the persisted TLS identity and
verify it again after restart. Full status includes both harness catalogs and
all capability/account readiness rows.

## Limits and dependencies

Speech and chat workers are wired through `compose.rs`. Packaged installations
must place `computer-mcp` beside the companion. Actual portal/X11/compositor
interoperability, native CLI inference protocol qualification and physical
Android operation require their own runtime evidence; synthetic tests do not
certify them.

The Linux composition uses `zbus = 5.19` (MIT), shared with the platform
adapter, and the internal crates `harness-claude`, `harness-codex`,
`computer-mcp`, `platform-linux`, `media`, `library`, `terminal` and `store`
(Apache-2.0); Tokio's `test-util` feature is enabled for tests. Adapter READMEs
retain their dependency/license notices.

## Voice and chat composition coverage

These tests cover owner-controlled voice and chat wiring:

| Test | Evidence |
| --- | --- |
| `voice_install_use_remove_live_over_owner_socket_and_tls` | Missing-install remedy on status and TLS refusal; streamed fixture install, capabilities events, live use/remove and an old session completing after removal |
| `owner_disconnect_cancels_voice_install_and_removes_staging` | Disconnect cancels an in-progress fixture installer, drains the owner operation and deletes staging |
| `token_stdin_permissions_redaction_status_doctor_and_offline_cli` | Actual CLI stdin token set; 0700/0600 checks; status/doctor/config/captured tracing contain no token; live enable/disable/exposure; unregistered-project rejection; offline use/remove/unmap |
| `telegram_composition_link_task_minimal_approval_capabilities_and_shutdown` | Token missing → connecting → connected → disabled; phone approves bot code over TLS; mapped Claude-fake task raises an app-only approval; active speech and channel shutdown releases sockets and process lock |
| `speech_replace_preserves_sessions_and_publishes_capabilities` | Hub engine replacement preserves active sessions; new sessions see unavailable remedy; capabilities events emitted |
| `speech_errors_preserve_busy_language_install_limits_and_redact_unknown_text` | Accurate admission errors and push/finalization `SpeechOut::Error`; arbitrary native content redacted |
| `speech_shutdown_drops_active_session_and_engine` | Held stream loses its session/old engine on Hub shutdown |
| `channel_availability_events_survive_host_refresh_but_not_restart` | Channel hook emits capabilities; unrelated host refresh preserves state; stale availability is never reused after restart |
| `cli_accepts_negative_group_ids_and_never_accepts_token_arguments` | Negative Telegram group IDs parse directly; token arguments are rejected |
| `config_rejects_unknown_channel_fields_and_untrusted_plaintext_endpoints` | Strict nested config and numeric-loopback-only plaintext endpoints |
| `token_source_rejects_unprotected_files_and_links_without_disclosing_token` | Owner-only reads reject permissive modes, symbolic links and hard links; errors/debug output redact values |

Adapter lifecycle and availability live together in `runtime.rs`. Channel
availability is scoped to the current boot. Pending link codes are
bot-displayed codes entered in the app through `approve_channel_link`.

The selected speech model is verified and opened on a blocking worker. Removing
or changing it permits current sessions to finish on their loaded engine; new
sessions use the replacement. Shutdown order is acceptance → channels → bridge →
hub. The channel workers restart together through the adapter's one handle;
the Hub and other adapters remain in place.

Direct external dependency: `rpassword = 7.4.0` (Apache-2.0), with its
dependency `rtoolbox = 0.0.6` (Apache-2.0). Internal `speech` and `channels` are
Apache-2.0. `Cargo.lock` records their resolutions.

These tests do not qualify a real model download, microphone, live Telegram or
Discord account, a physical phone or Windows runtime permissions. The speech
crate retains its opt-in model tests; the channels crate tests both platforms
against loopback servers.
