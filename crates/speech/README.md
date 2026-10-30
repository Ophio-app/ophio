# Local speech recognition

This optional adapter implements `ports::SpeechEngine` and `SpeechSession` on
Linux and Windows. Recognition uses the person's computer, accepts 16 kHz mono
PCM16 samples, and produces revisable text with approximate capture spans. It
never captures a microphone. Typed annotation does not depend on this adapter.

## Integration

```rust,no_run
let engine = speech::Installed::open(model_dir)?;
// Wire this Arc<dyn ports::SpeechEngine> into the companion's speech seam.
```

`open` verifies all files and loads `tiny.en`. `open_model(model_dir, "base.en")`
or `"small.en"` selects another explicitly installed model. Run these synchronous
constructors at startup or on a blocking worker. One loaded model and its four
CPU workers are shared by all sessions on that engine. Opening another engine
loads another model; keep one instance in the composition root.

Missing, unreadable, corrupt or unloadable models produce an engine with an
explanatory `Unsupported` or `PermissionMissing` availability. `start` then
returns `SpeechError::Unavailable`. Readiness includes the exact remedy:

```text
ophio voice install tiny.en
```

The command name comes from `ports::PROGRAM_NAME`. The composition root
owns the model-directory location, the CLI and its local-owner authorization.
There is no default config-directory name in this crate. English hints `en`,
`en-US`, `en-GB` and `English` are accepted; other language hints are rejected.
No automatic language detection is claimed by these English-only checkpoints.

The installer API is `install(catalog_entry, model_dir, progress).await`;
`remove(catalog_entry, model_dir)` removes only that model's known files, refusing
unknown contents and symlinks. Neither function is reachable through the speech
port. Expose them only as an explicit local-owner action. After installation or
removal, replace the engine and refresh capabilities. Already loaded weights
remain usable until their engine and sessions are dropped.

The services adapter forwards known, content-free `SpeechError` explanations
(busy, not installed, English only, size and duration limits) with matching
protocol error codes; any other native error text stays private.

## Engine and streaming policy

The engine uses Candle 0.9.2 (MIT OR Apache-2.0), CPU only, without BLAS, CUDA,
Metal or native inference components. A private four-thread Rayon pool runs
inference from Tokio's blocking pool. The FFT/mel frontend uses RustFFT and
computes Whisper's 80-bin Slaney log-mel representation without spawning threads.
Candle 0.9.2's matrix operations use the current Rayon pool. The newer 0.11.0
candidate creates process-wide pools configured through environment variables;
that prevents enforcing this adapter's independent thread cap. Treat an upgrade
as a resource-policy change and rerun the benchmarks.

* Audio is processed in 20 ms frames. An RMS energy gate of 200 PCM16 units after
  DC removal skips silence; it is not a learned VAD and cannot reliably distinguish
  quiet speech, music, fans or other background sounds.
* Voiced windows retain 100 ms of preroll and are decoded after each second of
  new audio. Greedy decoding is deterministic, bounded to 128 tokens per window,
  and detects repeated token phrases. Silence and low-confidence non-speech
  hypotheses can yield no text.
* An utterance is finalized after 320 ms of silence, or after 240 ms when two
  successive decodes agree on its text. `finish` decodes and finalizes any tail.
* Continuous speech has an eight-second window limit. At a forced boundary the
  current hypothesis becomes final; 500 ms of audio is retained as context for
  the next window. An exact normalized suffix/prefix match removes duplicated
  context words. This heuristic can misrecognize a word crossing a forced
  boundary; user review remains necessary.
* Each active utterance has one stable segment ID and increasing revisions.
  Empty final text retracts a discarded draft. Final segments never change.
  Start times are fixed; end times advance to the last voiced sample. New spans
  start at or after the previous final span. Silence remains part of the session
  clock, including before the first utterance. Times use sample counts only.
* Spans describe captured utterances, **not word alignment**. They are coarse
  suggestions for drawing association; explicit user links and edits take
  precedence. Large pushes are rejected instead of silently truncating audio.

There are two session slots and one inference job at a time per engine. A third
`start` returns `Unavailable` with a busy explanation. Each push accepts at most
16,000 samples (one second); a session accepts at most 9,600,000 samples (ten
minutes). Rejected input does not advance its clock. Buffered raw audio is bounded
by an eight-second window plus a push/remainder; the complete recording is never
retained here. Final transcript history is owned by the caller, not this crate.

Dropping a session or a pending push/finish future cancels work cooperatively.
Checks run between frontend frames and decoded tokens; a running tensor operation
finishes before cancellation is observed. An abandoned operation makes that
session unusable, preventing retries from silently losing audio or revisions.
A running worker keeps its session slot until it exits. The caller receives
backpressure rather than an unbounded inference queue.

## Privacy and installation

Recognition and availability never make network requests. Audio, transcript text
and language hints are not logged. Raw audio is never written to disk. Decode
caches are cleared between windows and sessions. The opt-in test/benchmark
programs deliberately print their supplied fixture transcript for evaluation.

Installation accepts only entries identical to the compiled catalog. HTTPS and
certificate verification are mandatory, including redirects. No account, token,
model-hub client or cached credentials are used. Redirect error URLs are not
included in errors. Downloads have bounded sizes and connection/total timeouts.

Files are downloaded into a temporary directory inside `model_dir`. Every file
must match its exact byte count and SHA-256 before the directory is atomically
renamed into place. A live destination is never intentionally replaced; remove
it explicitly first. Failed or cancelled downloads remove their staging directory
and preserve existing data. Files are flushed before publication; Unix directory
metadata is also synced. Startup independently hashes the exact bytes passed to
the model loader, rather than trusting a marker or reopening an unverified file.

Unix model directories/files have modes 0700/0600. Windows installation uses the
system Windows PowerShell, with no profile or visible window, to set a protected
DACL containing only the current user's SID before writing model data. Paths are
passed as environment data, never interpolated into PowerShell. Permission setup
failure aborts installation. Windows ACL runtime behavior still needs validation
on a Windows host; the implementation cross-compiles successfully. This is not a
security boundary against another process running as the same user.

## Pinned models

Weights, tokenizer and configuration come directly from the official
[OpenAI Whisper repositories](https://huggingface.co/openai/whisper-tiny.en).
The [Whisper code and model weights are MIT licensed](https://github.com/openai/whisper/blob/main/LICENSE).
No weights or audio fixtures are included in git or a package.

| Model | Installed bytes | Languages | Revision |
| --- | ---: | --- | --- |
| `tiny.en` (default) | 153,467,752 | English | `87c7102498dcde7456f24cfd30239ca606ed9063` |
| `base.en` | 292,809,504 | English | `911407f4214e0e1d82085af863093ec0b66f9cd6` |
| `small.en` | 969,399,630 | English | `e8727524f962ee844a7319d92be39ac1bd25655a` |

Each source URL is
`https://huggingface.co/openai/whisper-<name>/resolve/<revision>/<file>`.
The exact URLs and pins are also exposed by `catalog()`.

| Model/file | Bytes | SHA-256 |
| --- | ---: | --- |
| tiny.en/model.safetensors | 151,060,136 | `db59695928ded6043adaef491a53ef4e12da9611184d77c53baa691a60b958ad` |
| tiny.en/config.json | 1,937 | `b5366317df280f79e6cc366f259f379674f81a8721d458acf2e00680ccb10b1d` |
| base.en/model.safetensors | 290,401,888 | `d4dd5542fd6a1d35639e21384238f3bfe6c557c849d392b5905d33ee29e71db5` |
| base.en/config.json | 1,937 | `160c1df40a60d4ef5a6014d536fd845c33aabfd1f318458ac7f87cfe91ebbe76` |
| small.en/model.safetensors | 966,992,008 | `6014ac49b506df900f66f4aca6b0801eed7245594ace97bcaf73e0ae5b863066` |
| small.en/config.json | 1,943 | `3441094addfb2a31ef62ea117e95bd6e437bbad8e93d3697fe420a4a6d3603a5` |
| All three: tokenizer.json | 2,405,679 | `5eb60cec1e77aeeb6869a2bb5a8e01a84c3fe5d072d75369343021fe6f5310d0` |

## Verification and performance

See [BENCHMARKS.md](BENCHMARKS.md) for exact commands, transcripts, latency,
limitations and fixture provenance. `tiny.en` met the latency target in these
measurements and correctly recognized common terminal, Cargo and HTTP vocabulary;
it still made errors on the synthetic Rust compiler sentence. `small.en` improved
that sentence at higher cost but misrecognized Cargo. `base.en` gave no accuracy
benefit on that synthetic fixture. These two clips are a smoke test, not a
representative technical-speech accuracy evaluation or an eight-core laptop
certification. Typing and transcript review remain complete paths.

Model-free tests cover installation, cleanup, permissions, corruption, revisions,
finalization, overlapping-window time accounting, silence, limits and cancellation.
The model-backed test is inactive unless `SPEECH_TEST_MODEL_DIR` is set. No test
downloads a model automatically. Windows GNU/LLVM builds, including the release installer example, need neither
CMake nor native inference components; the Windows runtime is untested.

Direct dependencies and distribution licenses:

| Dependency | Declared version | License |
| --- | --- | --- |
| candle-core, candle-nn, candle-transformers | 0.9.2 | MIT OR Apache-2.0 |
| tokenizers | 0.22.2 | Apache-2.0 |
| serde_json | 1.0.150 | MIT OR Apache-2.0 |
| sha2 | 0.10.9 | MIT OR Apache-2.0 |
| reqwest | 0.12.28 | MIT OR Apache-2.0 |
| tempfile | 3.27.0 | MIT OR Apache-2.0 |
| rayon | 1.11.0 | MIT OR Apache-2.0 |
| rustfft | 6.4.1 | MIT OR Apache-2.0 |
| hound (test/example only) | 3.5.1 | Apache-2.0 |

The resolved dependency graph was checked for declared MSRV above 1.92 and
incompatible copyleft-only licenses; none were found. Where a transitive package
offers MIT/Apache alongside LGPL or Unlicense, use its MIT/Apache option.
