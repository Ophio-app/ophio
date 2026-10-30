# Bot token memory handling

`BotToken` stores its String in `zeroize::Zeroizing`, including during validation,
so successful construction and rejected input both wipe owned storage on drop.
Debug and Display redact it. `Http` retains one shared token until its last owner
drops, and zeroizes temporary Telegram URLs and Discord authorization strings.
Discord authorization headers are marked sensitive for HTTP debug formatting.
Gateway Identify/Resume serialization borrows the token and writes protected
buffers instead of making an intermediate JSON Value containing the token.

The owner CLI's `TokenInput` implements Zeroize/ZeroizeOnDrop. Piped input
reserves the complete bounded input size before reading, preventing secret
reallocations; the returned password String moves directly into `TokenInput`.
Owner request serialization uses a fixed-size zeroizing buffer before handing a
frame to the transport. Unix owner-file reads use bounded, preallocated zeroizing
buffers. Windows file reads and DPAPI plaintext outputs return Zeroizing;
the DPAPI allocation is wiped before LocalFree by the platform adapter.

The crates declare `zeroize = "1.9"` (MIT OR Apache-2.0). Tests use only synthetic tokens and loopback services. They
check redaction, protected storage, observable wrapper-drop erasure without
accessing freed memory, bounded stdin parsing, and gateway payloads.

## Copies outside the wipe boundary

The current third-party APIs retain allocations without a supported wipe hook:

- Telegram URL parsing/request creation: reqwest/url URL strings, HTTP URI copies
  and hyper request-line/header encoding buffers. Errors containing URLs are
  discarded and replaced with redacted channel errors.
- Discord HTTP authorization: http HeaderValue/Bytes and hyper's serialized
  header buffers. The sensitive bit redacts formatting; it does not erase bytes.
- Discord gateway: tungstenite's immutable UTF-8 message storage and its encoded
  WebSocket write buffers after Identify/Resume. TLS and socket layers can make
  further internal copies on both HTTP and WebSocket routes.
- Local owner IPC on Unix and Windows: Axum/tungstenite immutable UTF-8 frames,
  the String required by LinesCodec, and the codec's read/write BytesMut buffers.
  These include the inbound frame from which TokenInput is deserialized.
- Escaped input in serde_json can use an internal deserializer scratch buffer
  before constructing TokenInput. Its public API provides no erasure access.
- Piped stdin uses the standard library's internal stdin buffer. Hidden terminal
  entry uses rpassword/rtoolbox; its current password uses SafeString, but input
  character temporaries, discarded growth allocations and OS input buffers are
  outside the companion's erasure control.

The companion-owned buffers described above are wiped on ordinary destruction,
including error/cancellation paths. This is not a guarantee that every transport,
allocator, operating-system or abruptly terminated process copy is erased.
