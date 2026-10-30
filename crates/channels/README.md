# Chat front doors

This crate connects a person's Telegram or Discord bot to the same companion
`Hub` used by the phone. It does not run an agent, choose another account, or
provide desktop, terminal, Library, file-transfer or administration commands.
Product-owned code is Apache-2.0.

## Setup

1. Create **your own dedicated bot**: Telegram's BotFather, or a Discord
   developer application with a bot. Do not reuse a token owned by another
   gateway. The companion never imports credentials from installed programs.
2. Paste the bot token into the **companion's owner-only secret store**, never
   into a chat, this JSON configuration, a command-line argument, or source
   control. The composition root implements `TokenSource`; this crate never
   reads token files or environment variables.
3. Set `enabled`. Each chat works in one registered project. Choose it in the
   app on the linked chat's card in Connections › Chat, or map the numeric
   chat/channel ID to the project UUID with `ophio channels map`. Both edit
   the same configuration and reconnect the bot. Choose a default harness,
   model and effort for that project in the app. Each group/channel needs its
   own explicit mapping. Discord threads are separate channel IDs and need
   their own mapping and sender links.
4. For Discord, install the bot with `bot` and `applications.commands` scopes
   and allow View Channel / Send Messages. Keep privileged intents disabled.
   The connector registers its named slash commands without deleting unrelated
   commands. Use slash commands in guilds; ordinary reply text works in DMs.
   Telegram privacy mode can stay enabled: use commands and replies to the bot.
5. Write to the bot. It returns a short code and says:
   **Approve this code in the app: Connections › Chat**.
   The owner approves it there. Until approval, the message performs no task
   action. Every sender must link independently, including in groups. The
   computer gives each person one code a minute, each chat three and all chats
   together ten; a chat that asks more often is told to wait a minute. A chat
   whose code runs out unapproved is forgotten until it asks again. A linked
   chat with no project is told: **Choose a project for this chat in the app:
   Connections › Chat**.

Example configuration (IDs are placeholders):

```json
{
  "telegram": {
    "enabled": true,
    "projects": { "123456789": "00000000-0000-4000-8000-000000000001" },
    "exposure": "minimal"
  },
  "discord": { "enabled": false }
}
```

Missing fields default to disabled connectors and `minimal` exposure. Endpoint
settings are optional; their defaults use platform HTTPS/WSS endpoints. Plain
HTTP/WS is permitted only for numeric loopback test servers. Redirects and
ambient HTTP proxies are disabled. Never configure an endpoint you do not trust:
that endpoint receives the corresponding token.

The owner CLI, secret-store UI and phone connection screen are supplied by the
companion/client integration; this crate does not implement those screens.

## Integration

```rust,ignore
let handle = channels::spawn(
    hub, config, token_source, state_directory, shutdown_token,
).await?;
let current_status = handle.status();
// On orderly companion shutdown:
handle.shutdown().await;
```

`Config`, `ChannelConfig`, `Exposure`, `Endpoints`, `BotToken`, `TokenSource`,
`ChannelStatus`, `ChannelState`, `ChannelsHandle`, `Error` and `Result` are public.
`spawn_with_clock` accepts `Arc<dyn ports::Clock>` for deterministic admission
and reference-expiry tests. `TokenSource::token` returns
`Result<Option<BotToken>>`; `None` means token missing. Errors and token debug
output omit credential values and upstream bodies/URLs.

A handle reports disabled, connecting, connected, token missing, conflict or
platform error, with a local remedy. Dropping it cancels workers; explicitly
await `shutdown()` before closing the Hub. Use one stable, owner-only state
directory per companion. Unix permissions are set to 0700. The Windows
composition root/installer must provide an owner-only ACL. A process lock
prevents two connectors from opening the same directory.

## Commands and receipts

| Action | Telegram | Discord |
| --- | --- | --- |
| Start coding with mapped project defaults | `/new <prompt>` | `/new prompt:…` |
| Active tasks and attention | `/status` | `/status` |
| List up to 50 tasks | `/tasks` | `/tasks` |
| Next-turn message | Reply to a task acknowledgement | Reply in a DM, or `/reply task:<UUID> text:…` |
| Steer current work | Reply with `/steer <text>`, or `/steer <UUID> <text>` | `/steer task:<UUID> text:…` |
| Stop | Reply with `/stop`, or `/stop <UUID>`; confirm button | `/stop task:<UUID>`; confirm button |
| Answer a question | Reply to its notice | Reply in a DM, or `/answer message:<message ID> text:…` |

A task reference is its full UUID. Unknown commands, unrelated reply messages,
other projects, revoked senders and stale approvals cannot authorize effects.
A forwarded Telegram message is never a command, an answer or a button press;
in reply to a task it is passed on as ordinary text.
Reply associations belong to a particular linked sender, not everyone who can
see the message. Telegram forum topics inherit that sender's task after an
acknowledgement in the topic. If a topic contains multiple tasks, an explicit
reply or task UUID is required; the connector never guesses which one. Discord's Copy Message ID command may require Developer Mode.

The first response is **Received**. Its edit then says **Accepted by the
computer**, a typed refusal, or **Outcome unknown — check the app**. Acceptance
is never described as completion. Native steering/interrupt support and writer
conflicts remain decisions made by the Hub and native harness.

Command IDs are UUIDv5 identities over platform, bot ID and update/interaction
ID. The local SQLite journal records the original payload, defaults, revision
and expiry before submission. Button consumption and command intent commit in
one transaction. Retries use the original envelope; the Hub command journal
owns deduplication. SQLite WAL with FULL synchronization atomically persists
Telegram offsets, reply associations, button references and notification cursors.
State includes inbound task prompts for replay and platform identity IDs; it
contains no bot or interaction tokens. Keep it private and back it up with the
companion. Records are currently retained indefinitely.

## Platform disclosure and approval policy

Telegram/Discord receive messages you type, task references, states, link codes
and the bot's responses. Group/channel members can read those responses too;
individual linking does not make a group private. Chat linking is separate from
phone pairing and can be revoked separately. Revocation is checked on every
command and before each notification, and supplied grants are limited to tasks
and approvals even if an upstream identity mistakenly carries broader grants.

* `minimal` (default): task reference and state only. Approval notices direct you
  to the app; there are no buttons approving an effect you cannot inspect.
* `titles`: task titles and a short completion summary may be sent. Approval
  notices include the exact supplied effect, scope, detail and raw request.
  Choose this only if those contents may be shared with the platform and the
  mapped group's members. Commands, file edits, network and tool requests have
  Allow / Deny buttons. Questions accept answers. Unknown kinds, supervised
  computer-use approvals and effects too long to review in one message require
  the app. There is no truncated approval with a working Allow button.

Neither setting mirrors desktop pictures, terminal output, Library documents,
attachments, streaming agent prose or full task conversations. Completion
summaries are limited to 500 characters at `titles`. Errors expose stable codes,
not arbitrary native error bodies. Telegram uses escaped HTML; Discord escapes
Markdown, suppresses embeds and disables mentions. Messages split at 4096/2000
UTF-16 units without splitting a Unicode scalar.

Approval buttons carry only random, local references bound to sender, chat,
project, approval revision and host boot epoch. The Hub rechecks current grants
and approval state. Local confirmations expire after ten minutes; native
approval expiry also applies. A connector that starts with `minimal` exposure
deletes the approval references already in chats, so those buttons stop working
and don't work again if exposure later returns to `titles`. Resolution events edit the notice and remove its
buttons, displaying the recorded state and actor/location. The current protocol
can label a request `resolved_elsewhere` without supplying the actual decision;
in that case the connector explicitly reports that the decision was not supplied.
It does not guess Allow or Deny.

Notifications come from each linked identity's Hub subscription and are filtered
to its mapped project: approval, finished, failed and reconciliation required.
Live text is ignored. Subscription lag resubscribes from the saved cursor;
history gaps are disclosed and approval notices are rebuilt from a snapshot.
Platform message delivery is not transactional with local persistence: a crash
at that boundary can duplicate a notice, but cannot repeat a Hub command effect.

## Ownership, outages and limitations

Telegram uses long polling only. An existing webhook or HTTP 409 stops this bot
immediately with `conflict`. The connector never deletes a webhook, disables
another program or repeatedly fights for a token. Create a separate bot, or
review ownership yourself, then restart this connector. HTTP 429 delays use
`retry_after`. A saved offset advances only after handling the update.

Rate limits are kept per method and chat on both platforms. A wait of up to a
second is ordinary pacing and is waited out in place. A chat asked to wait longer
waits alone: its updates, replies and notices are tried again once the limit
ends, in order, while other chats' go ahead, and Telegram's saved offset stays at
the first update still waiting. The rest of a message already partly posted
waits rather than repost its first part. A limit the platform reports as global
holds up every chat.

Discord uses the Gateway with the unprivileged Direct Messages intent (4096),
slash interactions and components. Heartbeat ACK loss, reconnect events and
resumable disconnects reconnect using the session and sequence. Invalid sessions
identify again; fatal authentication/intent errors stop. REST observes returned
bucket deadlines, global rate limits and retry delays. **Discord does not offer
a reliable signal identifying another program using the same bot token.** The
local state lock cannot establish exclusive ownership on another computer. Use
a dedicated bot; no claim of cross-program conflict detection is made.

Discord resume state is in memory. A process restart creates a fresh session;
Discord does not replay every message sent while the bot was offline. An
interaction that cannot be acknowledged does not start a task. Check the app
before resubmitting work with a new interaction ID. Platform setup, permission
or state-storage failures may require restarting the connector after the local
remedy. Notifications are only available for chat tuples the connector has seen.

These tests qualify synthetic local API behavior, not live platform availability,
Windows filesystem power-loss behavior, or the owner's bot permissions. No live
bot token or public platform API is used by the test suite.

## Verification and dependencies

```sh
cargo fmt --all
cargo clippy -p channels --all-targets -- -D warnings
cargo test -p channels
cargo check -p channels --target x86_64-pc-windows-gnullvm
```

Unit/in-process integration tests use a fake Hub, fake clock and loopback Axum
Bot API/REST servers plus a loopback WebSocket Gateway. Coverage includes
linking, refusal, grant intersection, defaults, restart replay, Stop confirmation,
approval sender checks and stale replies, resolution edits, conflict shutdown,
429 delays, Gateway Resume, splitting/escaping, exposure and revocation.
The Telegram rate-limit scope test waits for the client to install the limit,
then uses paused Tokio time to check both chats before and after expiry. Loopback
I/O progresses without automatically advancing the clock, so a delayed 429
response cannot make the second chat start before the limit exists.

Direct dependency declarations (versions are compatible lower bounds):

| Dependency | Version | License |
| --- | --- | --- |
| uuid (`v5`, `v4`) | 1.26 | Apache-2.0 OR MIT |
| tokio-util | 0.7.18 | MIT |
| reqwest, rustls only | 0.12.28 | Apache-2.0 OR MIT |
| tokio-tungstenite, rustls only | 0.29.0 | MIT |
| futures-util | 0.3.31 | Apache-2.0 OR MIT |
| rusqlite, bundled SQLite | 0.38.0 | MIT; SQLite public domain |
| axum (tests) | 0.8.8 | MIT |
| tempfile (tests) | 3.24.0 | Apache-2.0 OR MIT |

Workspace dependencies are reused for protocol/ports/services, Tokio, Serde,
thiserror, async-trait and tracing. Retain applicable dependency notices when
packaging.

Implementation references: [Telegram Bot API](https://core.telegram.org/bots/api),
[Discord Gateway](https://github.com/discord/discord-api-docs/blob/main/developers/events/gateway.mdx),
[Discord interactions](https://github.com/discord/discord-api-docs/blob/main/developers/interactions/receiving-and-responding.mdx),
and [Discord rate limits](https://github.com/discord/discord-api-docs/blob/main/developers/topics/rate-limits.mdx).
