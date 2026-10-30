# Reproducing the speech evaluation

Measured on 2026-09-27 using Rust/Cargo 1.98.1 release builds on a Linux x86-64
desktop with a 16-core, 32-thread Zen 5 CPU and 64 GB of RAM, and a private pool
of **four inference threads**, while other work was running. This is not an
eight-core laptop measurement. No GPU, BLAS, network inference or paid service
was used. Cold load includes local hash verification; filesystem cache may be warm.

RTF here is the sum of the streaming push/finish execution times divided by
supplied audio duration. It includes repeated window decoding, frontend work,
queueing and finalization; it excludes model load and deliberate replay pacing.
Audio was fed in 200 ms chunks at real-time deadlines. First partial is elapsed
wall time from sample zero, including capture accumulation. Maximum delivery lag
is wall time beyond the capture end of the push that produced a segment. It does
not assert word-level timestamp accuracy. One second of trailing silence is
included in each fixture's supplied duration.

## Final streaming measurements

| Fixture/model | Audio s | Load s | Compute s | RTF | First partial s | Max delivery lag s |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| JFK / tiny.en (default) | 12.000 | 0.128 | 1.241 | 0.103 | 1.258 | 0.196 |
| Technical / tiny.en | 11.184 | 0.130 | 1.247 | 0.111 | 1.084 | 0.172 |
| Technical / base.en | 11.184 | 0.322 | 2.492 | 0.223 | 1.161 | 0.611 |
| Technical / small.en | 11.184 | 0.738 | 4.609 | 0.412 | 1.239 | 0.746 |

Default-model JFK transcript (ten partial updates):

> And so my fellow Americans ask not what your country can do for you. ask what you can do for your country.

Default-model technical transcript:

> The button is disabled. Open the terminal and run cargo test. The request returned status 500. Ficks must compile error in the parser.

Base technical transcript:

> The man is disabled. Open the terminal and run Cardo test. The request returns status I'm hungry. Fix the rust compiler ever in the parser.

Small technical transcript:

> The button is disabled. Open the terminal and run Cardo test. The request returned status 500. Fix the Rust compiler error in the parser.

`tiny.en` was selected as the smallest usable starting point: it recognized the
common terminal/Cargo/request phrases, had the lowest cost, and met the 1.5-second
first-partial target on both fixtures. Technical names still need review. The
synthetic voice is not representative of all speakers, microphones or accents;
these results do not establish a broad accuracy score. `small.en` is an explicit
quality/cost option, not a silent automatic upgrade.

The finalization policy uses 320 ms of silence, or 240 ms plus agreement. A
600 ms setting joined the synthetic sentences across the eight-second window
limit and duplicated or misrecognized boundary words. The transcript errors above
are shown as measured.

The early Candle 0.9.2 direct-decoder probe (not the streaming benchmark) decoded
5 s in 0.073 s and 11 s in 0.208 s. A Candle 0.11.0 probe decoded those windows in
0.094/0.245 s, but its environment-configured global pools did not meet the
per-engine resource isolation requirement. Whisper.cpp was not benchmarked:
Candle already met the measured latency target and avoided native inference
packaging. The speech build does not use CMake.

## Fixtures and licenses

The recognition fixture is the 11-second JFK inaugural-address excerpt distributed
with whisper.cpp, an excerpt of the January 20, 1961 U.S. federal-government
recording. It is public domain; see the
[JFK Library source](https://www.jfklibrary.org/asset-viewer/archives/usg-17) and
[public-domain recording identification](https://commons.wikimedia.org/wiki/File:JFK_inaugural_address.ogg).
The exact [mirror revision](https://github.com/ggml-org/whisper.cpp/blob/b0a11594aec50892a02cd8d129eee2dfe93a8bb8/samples/jfk.wav)
is pinned in the preparation script. Its 352,078 bytes have SHA-256
`59dfb9a4acb36fe2a2affc14bacbee2920ff435cb13cc314a08c13f66ba7860e`.

The technical fixture uses locally installed eSpeak (`en-us`, 155 words/minute)
and FFmpeg to produce 16 kHz PCM16 from this newly authored, CC0-dedicated text:

> The button is disabled. Open the terminal and run cargo test. The request returned status five hundred. Fix the Rust compiler error in the parser.

The generated waveform is also dedicated to CC0-1.0. The measured waveform had
325,982 bytes with SHA-256
`b2059a86bfb7f9d82c280ca979356d422de15498bd9bf344d6fa5c4878cbb928`.
Different synthesizer versions can produce different bytes. No microphone or
real desktop interaction was used. Both waveforms and all model weights live in
an ignored cache, never in source control.

## Commands

From the workspace root:

```bash
python3 crates/speech/scripts/prepare_fixtures.py .cache/speech-models
cargo run -p speech --release --example models -- install tiny.en .cache/speech-models
cargo run -p speech --release --example models -- install base.en .cache/speech-models/installed
cargo run -p speech --release --example models -- install small.en .cache/speech-models/installed

SPEECH_TEST_MODEL_DIR="$PWD/.cache/speech-models" \
  cargo test -p speech --release --test model -- --nocapture

SPEECH_TEST_MODEL_DIR="$PWD/.cache/speech-models" \
SPEECH_TEST_WAV="$PWD/.cache/speech-models/technical.wav" \
SPEECH_TEST_EXPECT='button disabled terminal cargo test request status 500 Rust compiler error parser' \
  cargo test -p speech --release --test model -- --nocapture

# Repeat the technical command with these model-directory/model selections:
# SPEECH_TEST_MODEL_DIR="$PWD/.cache/speech-models/installed" SPEECH_TEST_MODEL=base.en
# SPEECH_TEST_MODEL_DIR="$PWD/.cache/speech-models/installed" SPEECH_TEST_MODEL=small.en
```

The `models` example installs through the engine's real `install` API, including
progress, size/hash verification and atomic publication.
The example refuses to overwrite an existing installation; skip already
installed entries or explicitly use its `remove` action before reinstalling.

## Checks

```bash
cargo fmt --all
cargo clippy -p speech --all-targets -- -D warnings
cargo clippy --workspace --all-targets -- -D warnings
cargo test -p speech
# Full workspace tests with an isolated runtime directory and no real display:
env -u DISPLAY -u WAYLAND_DISPLAY -u DBUS_SESSION_BUS_ADDRESS \
  -u PULSE_SERVER -u PIPEWIRE_REMOTE XDG_RUNTIME_DIR="$PWD/target/test-runtime" \
  cargo test --workspace
cargo check -p speech --target x86_64-pc-windows-gnullvm
cargo build -p speech --release --example models --target x86_64-pc-windows-gnullvm
```

The opt-in model test returns without running inference when its environment
variable is absent. Test duration and recognition output are reported by the
opt-in benchmark only.

The Windows linker warns that `-no-pie` is unused; the warning is harmless. The
Windows PowerShell ACL path and actual inference are untested on Windows. The
engine needs no CMake.
