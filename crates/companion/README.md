# Øphio on the computer: transport and CLI

This crate implements the host's TLS front door, five independent WebSocket
channels, local owner interface, and CLI. Application state is accessed through
`services::api::Hub`. No transport creates a second device database or command
journal.

## Integration status

`ophio run` builds one durable `CompanionHub` through `compose::ApplicationFactory`.
The same hub serves the phone, the local owner socket and the per-run tool bridge.
The factory supplies the persisted identity's host id; an existing store with a
different id is refused with a restore instruction. Library state lives under
`Paths.data/library`, separate from all registered roots; instruction discovery
uses the owner's home. Hub state lives under `Paths::database()`.

The Linux composition installs Claude Code, Codex, native PTYs, `FsLibrary`,
`TileEncoder`, and the private-address HTTP preview checker. It resolves
`computer-mcp` beside the `ophio` executable. Build and install both binaries:

```sh
cargo build -p companion -p computer-mcp
```

The Claude adapter's own approval relay wraps the hub bridge handler. Native
permissions become one durable approval with a waiting broker reply. Allow and
Deny complete that reply; there is no auto-allow fallback when the executable
is missing. Missing bridge installation is visible in readiness and prevents
managed Claude starts. Codex Allow is a single native effect; turn/session
permission grants are refused. Each run token binds one bridge connection;
disconnect consumes that route and expires its pending approvals.

Desktop capture and input default to disabled. `ophio desktop enable` is
an owner-only opt-in, recorded once sharing has opened; a refused dialog or a
desktop that cannot be shared leaves it off and says why. `ophio desktop disable`
closes sharing and forgets consent, whether or not Øphio is running, even when
the setting cannot be saved, the change cannot be recorded, or another setting
has become invalid, such as a removed Library folder. Settings that cannot be
read, or that sit in a folder open to other people, are left as they are. It
says what it could not do, and says first when input may not have been
released. Selection uses explicit process environment endpoints: advertised
native Wayland protocols, otherwise the session portal, or X11 in an X11
session. Only the portal path requests local desktop consent.
While that dialog is open, settings reads still answer; interrupting the
command or stopping Øphio abandons the request instead of waiting for it.
Restore tokens stay in the private data directory. Disable withdraws managed
observations, releases input on every screen, ends streams with a reason and
closes portal sessions. It applies immediately without restarting. Enabling in
a headless session reports how to start Øphio in a graphical login session.

`ComposeOptions` injects environment, executable directory, clock, harness
executables, desktop factory and a speech factory for isolated tests. Local
voice uses one selected, verified engine loaded on a blocking worker. Models
live in the private `Paths.data/voice` directory; selection defaults to `tiny.en`.
Install selects its model; install, remove and use apply immediately to new sessions. Existing sessions
finish with their loaded model, including when its files are removed. With no
selected model installed, the exact remedy is `ophio voice install tiny.en`.
Recognition never downloads models or sends audio to an external service.

Telegram and Discord run through the same Hub. Their validated configuration
lives in the `channels` section, using the channels crate's schema and default
`minimal` exposure. Owner commands restart the channel workers after draining
the old handle; runs, terminals and desktop sessions keep their existing state.
Connector status changes publish host capabilities and readiness immediately.
`status --json` and `doctor --json` additionally include `voice` (selected model,
availability, remedy) and `channels` (connector state, local remedy).

Shutdown stops network/owner acceptance and drains connections, then stops
channels, drains the bridge, shuts down hub runs, speech sessions, terminals and
media, and releases the instance locks. An unexpected bridge listener failure
stops the host. Transient TCP and owner-socket accept failures use bounded retry
backoff.

Windows cross-compiles. There, `platform-windows` supplies owner-only ACLs for
private folders and files, current-user DPAPI for the identity and chat tokens,
the owner pipe, which admits only the same user's processes, and the logon
registration `service install` writes. There is no insecure TCP or
ordinary-file fallback. `SecretStore` is the storage replacement interface.

## Identity and listeners

The first successful identity initialization creates a stable host UUID and an
ECDSA P-256 self-signed certificate. SANs include the machine hostname,
loopback, discovered tailnet DNS/IP names, and explicit listener IPs. The
certificate fingerprint is lowercase SHA-256 of DER. Existing identities are
loaded, not silently regenerated when addresses change.

TLS uses rustls 0.23 with the ring provider, TLS 1.3 only, and no early data. Ring
keeps the Linux and LLVM/MinGW builds free of an additional CMake-based crypto
build. Device authentication proves Ed25519 possession over the protocol's
length-prefixed transcript, including the per-session TLS exporter, both random
nonces, host, device, and endpoint kind. The total WebSocket authentication
window is ten seconds. Unknown, revoked, malformed, incompatible, replayed, and
misbound proofs fail closed.

Linux identity writes are atomic, synced, and owner-only. Reads reject symlinks,
hard links, foreign ownership, oversized identity files, and group/other access.
State directories are mode 0700 and the key bundle is mode 0600. Secret buffers
are zeroized where owned by this crate; no request bodies, signatures, exporters,
keys, input bytes, or media bytes enter tracing logs.

Discovery only executes `tailscale status --json`. It never reads credential
files, changes tailnet settings, or configures Serve/Funnel. When Tailscale is
unavailable, the listener remains on loopback unless explicit LAN addresses are
enabled. Address discovery is performed at startup; restart after a network
address change. There is no automatic all-interface fallback.

Configuration lives in the platform directory selected by `directories`:

```toml
port = 47100
listen = []
allow_lan = false
retention_days = 30
desktop_enabled = false
voice_model = "tiny.en"
roots = []

[channels.telegram]
enabled = false
exposure = "minimal"

[channels.discord]
enabled = false
exposure = "minimal"
```

For LAN access, set `allow_lan = true` and explicitly list the desired local IPs.
A wildcard is accepted only when explicitly listed with that opt-in. Prefer
specific interfaces. Application naming is centralized in `config::APP_NAME`;
the pairing scheme derives from it. Cargo's package/binary declaration remains
in the manifest as required by Cargo.

## Channels and recovery

`GET /v1/info` exposes only the host name, supported protocol version, and
certificate fingerprint. `POST /v1/pair` validates framing/version/key material
and forwards enrollment to `Hub::pair_device`; the hub owns single-use secrets,
expiry, persistence, and grants. Native endpoints reject browser Origin headers
and expose no permissive CORS policy.

Each of `/v1/control`, `/v1/media`, `/v1/terminal`, `/v1/transfer`, and `/v1/speech`
first exchanges control `Hello`, `Challenge`, `Authenticate`, `Authenticated`
frames, then switches to its channel-specific protocol. TLS export label is
`EXPORTER-device-auth-v1`, with 32 bytes and no context. Every connection has its
own proof; successful authentication calls `device_seen`.

Control responses and events have separate bounded queues. Receipts, input
acks, and pongs take priority. The event pump independently fetches replay,
sends `CaughtUp`, and filters duplicate live sequences. Broadcast lag
resubscribes from the queued cursor. On event-queue overflow,
`SubscriptionClosed { after_seq, reason }` terminates only that subscription and
discards its remaining queued events. The connection still accepts commands;
resubscribe from the supplied last-delivered sequence. `JournalGap` is reserved
for history the hub really no longer retains and requires loading a snapshot.
A physical socket that stops accepting writes still has a bounded write timeout;
receipt delivery cannot be guaranteed to a client that is not reading.

Ordinary control commands and queries execute in arrival order, with one active
request and at most 128 waiting (including reconnect history reads). Slow download preparation and Library reads
leave the reader free to admit Stop, Force stop and desktop interventions, handle
input and lease renewals, and answer heartbeats. Those replies may overtake an
earlier request; clients match command and query IDs. A full request queue returns
`LimitExceeded` for excess commands and queries without closing the connection or
preparing files. The hub records command rejections and preserves existing
deduplicated receipts. Authorization is rechecked when queued work starts;
command expiry includes the time spent waiting. Disconnect, revocation and
shutdown discard queued work and cancel unfinished request futures. Effects
already admitted to the hub retain their existing durable command lifecycle.
Retries of queued or preparing commands share the original response and receipt
time without taking another queue slot. Reusing a pending ID with a changed
request returns a conflict without rejecting the original command. Cancelled
download preparation removes its staging file after any filesystem operation
already in flight finishes.

The control heartbeat expires after thirty seconds without inbound traffic.
Control closure calls `control_disconnected`. Current device authorization is
looked up before operations, and idle/streaming connections are rechecked every
250 ms. Grant changes close an existing stream so the client must reauthenticate;
revocation stops the stream. Hub implementations must continue enforcing current
grants and lease fencing inside their own operations.

Media has at most the requested number of unacknowledged frames. It retains at
most one pending full keyframe. If a delta is dropped, a keyframe is requested
and dependent deltas are skipped until a complete image can be restored. Acks
must name a sent frame; acknowledgements are cumulative through that sequence.
Terminal output carries the protocol's big-endian 8-byte offset; detaching leaves
the terminal alive. Transfers honor negotiated resume offsets and the 256 KiB
chunk limit. The hub verifies upload size/hash and performs publication. Speech
accepts even-length PCM16LE, 16 kHz mono, at most one second per binary message;
finish and cancel are forwarded and disconnect cancels unfinished recognition.

All JSON messages are limited to 1 MiB (pairing HTTP bodies to 16 KiB). Transfer
and terminal input chunks are limited to 256 KiB; media output to 16 MiB. The
server bounds both TLS and upgraded connections. Shutdown tracks upgraded
sessions and runs resource cleanup before returning.

## Owner operations

On Linux, owner IPC uses newline-delimited JSON at `Paths::socket()`, in a private
directory, mode 0600. Both ends check peer UID using the Unix socket credentials;
control commands are attributed to `LocalOwner`. Local-only frames support
pairing, desktop settings, roots, retention, local voice installation and chat settings. These frames are never accepted
on the remote control connection. Device-auth frames are not required here.

Owner commands:

```sh
ophio run
ophio pair                          # asks you to confirm the device that answers
ophio pair confirm <code>           # confirm it without a terminal, as from a script
ophio status                        # what works and what needs attention; --json for records
ophio devices                       # names, permissions, last seen; --json for records
ophio devices allow <device> <permission>...   # e.g. control terminal, or all
ophio devices deny <device> <permission>...
ophio devices revoke <device>        # name or ID; it must pair again
ophio projects add /path/to/project
ophio projects list
ophio doctor                        # every area, then what needs attention; --json for all checks
ophio desktop enable
ophio desktop disable
ophio desktop status
ophio roots add /path/to/notes --label Notes --writable
ophio roots list
ophio roots remove <root-uuid>
ophio retention set 30
ophio voice install tiny.en
ophio voice use tiny.en
ophio voice status
ophio voice remove tiny.en
ophio channels token set telegram
ophio channels token clear telegram
ophio channels enable telegram
ophio channels map telegram <chat-id> <project-uuid>
ophio channels unmap telegram <chat-id>
ophio channels exposure telegram minimal
ophio channels disable telegram
ophio channels status               # every channels command also takes --json
```

`ophio pair` shows the pairing link, the first 128 bits of the certificate
fingerprint and a QR code. When a device asks to pair, it prints the device's
name and platform and a six-digit code, which the device's screen shows too.
Nothing is paired until you confirm. In a terminal it asks `Pair <device>? [y/N]`;
any answer but yes refuses the device, and a refusal ends that pairing code.
Without a terminal, for example when a script runs `ophio pair` in the
background, it prints the command to confirm with, `ophio pair confirm <code>`,
and waits. Check the code on the device before you confirm. One device can ask
at a time, the code expires after five minutes, and `ophio pair` exits with
success only when a device paired.

Replace `telegram` with `discord` for that connector. Token set prompts without
echo on a terminal, or reads one line from stdin. Tokens are never command-line
arguments or configuration fields. The private `Paths.data/secrets` directory
has mode 0700; atomic token files have mode 0600. Reads reject symbolic/hard
links, foreign ownership and group/other permissions. On Windows, tokens are
sealed with current-user DPAPI in files whose ACL admits only the owner and
SYSTEM.

Voice installation is an explicit owner action and streams download progress.
Ctrl-C or owner-socket disconnection cancels an unfinished download and removes
its staging directory. Only reviewed catalog models are accepted; hashes are
verified by the speech adapter. The installer is never reachable through a
phone or chat command. When the service is stopped, voice and channel commands
operate under the instance lock and report that the next start applies changes.
Live chat mapping rejects project IDs that are not registered in the Hub.

Send a message to your dedicated bot, then enter its link code in the app to
approve that sender. Every sender needs a separate link. `minimal` exposure
keeps approval decisions in the app; `titles` may disclose task titles and full
approval effects to the platform and chat members. See the
[channels setup and disclosure policy](../channels/README.md).

`pair` prints a copyable link, the fingerprint, an expiry countdown and a
terminal QR code, last so that it sits nearest the cursor, then waits for a new
device. The offer is intentionally shown to the owner on stdout, never in logs.
The link format is `ophio://pair?d=<base64url-json-offer>`. The QR code carries
the same offer packed as `ophio://pair?c=<base32>` (`pairing_qr_link`), about
half as long, so that the code fits a default-sized terminal window; when a
field does not fit the packed form, it carries the full link. The phone reads
both.

Roots and retention apply to the running hub through owner IPC, and config is
saved atomically before acknowledgement. Root additions emit
`LibraryRootsChanged`; removal immediately removes authorization. Root paths
must exist, are canonicalized, and cannot overlap any private Øphio
directory. Write access requires `--writable`. With the service stopped, these
commands save config and explicitly report that the next start applies it.
`status`, `doctor` and `channels` print a short summary: each item's state in
words and, for anything not working, the problem and what to do. `doctor` ends
by saying whether anything needs attention. `projects`, `roots`, `inbox`,
`desktop`, `retention`, `voice` and `jev` print the resulting projects,
folders or setting in the same way. Every one of these commands takes `--json`
for scripts: `status --json` includes the snapshot, harness catalog and
readiness; `doctor --json` reports all host and harness capability checks and
remedies; `projects add --json` prints the command receipt and `projects list
--json` the registered projects; the settings commands (`roots`, `inbox set`,
`desktop`, `retention`) print the desired config, `applied_live`, and current
desktop availability. Neither form shows tokens, keys or pairing offers, and the
summary also leaves out account names.

`service install` explicitly writes the managed systemd **user** unit, reloads
the user manager, and enables/starts it. If `graphical-session.target` is active
at installation, startup and shutdown follow the desktop session. Otherwise it
uses `default.target` and explains that desktop access requires starting
`ophio run` from the desktop's own autostart. The desktop must supply its
environment to the user manager. Re-installing stops the managed service,
removes old startup links, replaces the unit, and starts it again. The service
preserves ordinary session privileges for terminals and agents. It delegates
its cgroup to Øphio, which gives each agent run a cgroup of its own inside it,
so Stop and the end of a run end everything the run started.
`service uninstall` stops/disables and
removes only a unit carrying the generated ownership marker. `service status`
queries the user manager. Nothing installs automatically. Tests render units
and inject systemctl results; they never change the real session. Service status
uses systemctl's exit status, including a nonzero status for inactive services.

On Windows, `service install` registers Øphio to start at logon in the current
user's HKCU Run key and starts it now, in the background with no window, then
reports whether it came up. From an administrator's terminal it only registers,
and Øphio starts at the next logon, so it never runs elevated. When Windows gives
`ophio run` a console of its own, at logon or from a double-click, it starts a
windowless copy of itself and lets that console close. `service uninstall` asks
the running Øphio to quit over the owner pipe, waits until that process has
ended so its files can be replaced, and removes only the registration it owns.
Double-clicking `ophio.exe` with no command explains how to set it up. Help and
messages on Windows say `.\ophio.exe`, which is what PowerShell needs from the
folder.

On Linux, `desktop disable` closes the current portal session and forgets its
saved consent token, including when changing settings while Øphio is stopped.
Enabling portal sharing again asks for consent on the desktop.

To rotate the certificate, stop the service and run:

```sh
ophio rotate-certificate --confirm-repin
```

The host UUID and enrolled device records remain unchanged. **Every phone must
verify and re-pin the new certificate** before reconnecting, or explicitly pair
again. The CLI prints the new fingerprint. Never teach the client to accept a
changed fingerprint automatically. Keep the service stopped if a rotation
failed; the prior bundle survives an unsuccessful atomic replacement.

`uninstall-data` previews the owned paths unless `--yes` is supplied. Stop and
uninstall the service first. It removes these entries from its data folder:
`identity.json`, `accounts`, `voice`, `channels`, `secrets`, `db`, `db-wal`,
`db-shm`, `artifacts`, `staging`, `logs`, `library`, `desktop-consent`,
`voice-browser`, `agent-browser`, `setup.json`, `setup-skills`, `agents`,
`reporter.key`, `report-trail.json`, `report-crashes.json`, and `run`.
It also removes `config.toml` from its config folder and temporary files left
by interrupted report, identity, setup, or config writes in these two folders.
Received files in the old `db/inbox` folder are preserved. Registered projects,
native accounts, and Tailscale are never traversed. It removes the empty
config folder while still holding its instance lock, then removes
`instance.lock` and the data folder if empty. Other unrecognized entries,
and the folder holding them, are left alone. A start that races this cleanup
retries opening and validating its lock, recreating the data folder if needed,
rather than holding a lock on a file no longer there.

## Validation

Tests use isolated directories, loopback, fake hub implementations, and an
injected `ports::Clock` for authentication/heartbeat/pairing expiry. They cover
wrong pins, key/channel/exporter binding and replay, revoked access, one-time
pairing, journal replay/live/lag/overflow, a 100,000-event flood with a command
receipt deadline of two seconds, media window behavior, terminal framing,
transfer resume/chunk limits, speech framing, and owner UID rejection.

Run `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`,
and `env -u WAYLAND_DISPLAY -u DISPLAY -u DBUS_SESSION_BUS_ADDRESS cargo test --workspace`.
Build `computer-mcp` first when running only the `companion` crate's integration tests. Windows builds use the LLVM/MinGW toolchain
from `BUILDING.md` and `cargo build --target x86_64-pc-windows-gnullvm -p
companion`. These checks do not qualify Windows or physical Android clients. The
companion's tests have also run natively on Windows 11 in a virtual machine,
along with the packaged archive and an Android emulator;
`crates/platform-windows/README.md` lists what ran there and what has not.

`tests/composition.rs` uses the real factory, persisted TLS identity, device
pairing and authenticated WebSockets with authored Claude/Codex executables.
It exercises both Claude approval decisions, native stop, PTY echo, live roots,
Library save/conflict/revocation, collision uploads, downloads, loopback previews,
Linux desktop composition with a synthetic driver, and shutdown with active
resources. A subprocess test exercises the installed CLI paths with isolated
XDG directories and no display/D-Bus environment. No real inference, desktop,
network configuration or service installation is used.

## Dependencies

Versions below are the explicit manifest requirements; `Cargo.lock` records exact
resolution. All are compatible with Apache-2.0 distribution; preserve their
license notices when packaging.

| Dependency | Version | SPDX license |
|---|---|---|
| axum | 0.8 | MIT |
| base64 | 0.22 | MIT OR Apache-2.0 |
| clap | 4.5 | MIT OR Apache-2.0 |
| directories | 6.0 | MIT OR Apache-2.0 |
| ed25519-dalek | 2.2 | BSD-3-Clause |
| futures-util | 0.3 | MIT OR Apache-2.0 |
| hostname | 0.4 | MIT |
| hyper | 1.6 | MIT |
| hyper-util | 0.1 | MIT |
| rpassword | 7.4.0 | Apache-2.0 |
| rtoolbox (transitive) | 0.0.6 | Apache-2.0 |
| qrcode | 0.14 | MIT OR Apache-2.0 |
| rcgen | 0.14 | MIT OR Apache-2.0 |
| ring | 0.17 | Apache-2.0 AND ISC |
| rustls | 0.23 | Apache-2.0 OR ISC OR MIT |
| rustix (Unix) | 1.0 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |
| sha2 | 0.10 | MIT OR Apache-2.0 |
| tempfile | 3.20 | MIT OR Apache-2.0 |
| tokio-rustls | 0.26 | MIT OR Apache-2.0 |
| tokio-util | 0.7 | MIT |
| toml | 0.9 | MIT OR Apache-2.0 |
| tracing-appender | 0.2 | MIT |
| tracing-subscriber | 0.3 | MIT |
| zbus (Linux composition) | 5.19 | MIT |
| zeroize | 1.8 | Apache-2.0 OR MIT |
| tokio-tungstenite (tests) | 0.29 | MIT |
| nix (Linux tests) | 0.29.0 | MIT |

The internal `speech` and `channels` crates are Apache-2.0.
Existing workspace dependencies additionally used here are `serde`, `serde_json`,
`thiserror`, and `async-trait` (MIT OR Apache-2.0), `bytes` (MIT), and the internal
`ports` crate (Apache-2.0). `tokio` and `tracing` are also used.

## Interrupted Linux updates

Updates and rollback keep `~/.local/opt/ophio/bin/ophio` runnable by exchanging
program directories atomically (`renameat2(RENAME_EXCHANGE)`). Filesystems that
do not support that operation fail the update without moving the programs away.
A prepared replacement and a complete hard-linked snapshot of the current
release are recorded in `replacement.json`. Each entry's device and inode make
recovery repeatable after termination immediately before or after a move.
Upgrades exchange the programs first, so an older install stays intact until
the new executable can finish recovery. Rollback moves apps and version metadata
first, so an executable that predates startup recovery finds its matching files.

The installed command finishes a pending replacement before parsing commands
or starting the service, and executes the recovered binary if the version
changed. It holds the update lock throughout recovery. Staged downloads run
recovery under their apply lock. Startup resolves symlinked home/install
ancestors and checks whether another launch moved its executable aside, even
after that launch removed the journal. A displaced launch executes the installed
program so it finds its helpers in the live program directory. A launch with no
pending journal does not wait for the update lock, which the updater retains
while starting and checking the service. An older executable without startup
recovery can still launch on either side of the program exchange; a subsequent update
using this updater cleans any remaining replacement files. Process termination is
covered; abrupt power loss and filesystem durability are not qualified here.

`cargo test --locked -p companion --lib update::linux::tests` covers update and
rollback, including SIGKILL at every replacement move and cleanup boundary.
The subprocess tests restart through both the normal command link and the
service executable path, then launch again to check recovery is repeatable.
They repeat the interruption cases through a symlinked home. Concurrent-start
tests stop one launch before recovery or executable lookup, let a second finish,
then verify the first uses the installed executable and helper directory. This
covers both distinct and shared executable inodes. Separate cases verify startup
under the updater's lock and keep staged updaters out of installed recovery.
The concurrent-start checkpoint raises SIGSTOP on the test's executing thread.
A process-wide signal can reach another test-runner thread while recovery
continues into exec, losing the intended checkpoint.
The parent requires an observed SIGSTOP before launching the competing recovery,
then resumes the first launch with SIGCONT and requires it to finish successfully.
They use isolated homes and do not restart the computer's real service.
`cargo test --locked -p companion --test update_recovery` checks the actual CLI
binary, including recovery before `--version` and the service path's `run --help`.
No real service restart is performed by these tests.
