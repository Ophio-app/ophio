# Reproduction clips

Import a video with the existing picker or share intake, choose a range of up to
60 seconds, then choose **Create clip and key frames**. Creation is local and
produces a new silent clip and three PNG frames. The original stays in the draft.
A clip is not a selection of timestamps: the exporter must produce decodable
bytes with a verified duration before any derivatives enter the draft.

Android uses app-private staging and `MediaExtractor` / `MediaMuxer`. It accepts
one AVC, HEVC or MPEG-4 video track, starts at the preceding sync sample, and ends
at the next sample boundary. Review states the actual start and end, including
any keyframe adjustment. Audio tracks are omitted. Encrypted, partial,
non-monotonic or oversized samples are refused. Unsupported clips retain the
screenshots and individual-frame alternative. No Android permission is
requested.

The browser uses HTML video decoding and WebCodecs VP8 encoding at 15 frames per
second, with a maximum dimension of 1280 pixels. The finite WebM writer verifies
continuous timing and complete duration. Output frames are decoded from that
export, not from a second interpretation of the selected range. Neither path
uses screen capture, microphone capture, remote transcoding or inference.

Each derivative retains the original source key, SHA-256 and size, the requested
and actual range, its own hash and size, and its creation method. Clip ranges,
frames, marks and steps persist using the existing annotation store and IndexedDB.
A cancelled or failed encode publishes no partial derivative set.

Review distinguishes two explicit deliveries:

- **Send clip to computer** transfers the reviewed clip as a separate artifact.
  The computer's confirmed artifact id and resumable transfer id stay in the
  draft. The current agent adapters cannot receive video in annotation bundles.
- **Review and send** sends the selected still images and authored steps through
  the existing annotation flow. It does not include the original video or audio.

Both paths hash the local bytes before declaring any transfer. Image bundles
preflight every selected source before the first upload. The immutable Blob that
passed that check is also the Blob handed to the uploader. Previously uploaded
sources with local bytes are checked again. A final host hash confirmation is
required before showing that the computer saved the clip.

## Verification

The rerunnable headless plan is `client/scripts/plans/reproduction-clips.json`. When
`node_modules` is a symlink, start Vite from `client` with:

```sh
npx vite --config src/screens/annotate/vite-check.config.mjs --mode sim --host 127.0.0.1 --port 5913 --strictPort
node scripts/shots.mjs http://127.0.0.1:5913 /tmp/reproduction-shots scripts/plans/reproduction-clips.json
```

It imports authored eight-second footage through the actual picker handler,
exports 0:02–0:05, checks the derivative hashes and frame ranges, exercises
explicit transfer and corruption refusal, and reloads a marked draft before
review. It covers light/dark, large text, landscape, wide, empty, disconnected,
revoked and denied states. The fixture contains no captured desktop material.

Logic tests cover atomic publication, storage/decode failure, range edits during
export, persisted marks and ranges, corrupted bytes before upload, final transfer
failure recovery, native result validation/cleanup, and missing/reordered encoded
frames. Rust tests check native IPC input bounds.

The Android plugin's `ClipChecks` instrumentation generates synthetic AVC frames
and checks remuxed duration, sync-sample start, output hash, tampered input,
malformed input, bounds and cleanup. The existing instrumentation runner selects
only these checks with the `clips=true` argument. They require an Android device;
compiling the instrumentation does not execute the media checks.

Android packaging and Rust shell tests do not qualify actual Android decoding or
remuxing. Physical picker/share import, rotation, unusual encodings and playback
of native keyframe cuts still need device qualification. Desktop recording is
unavailable: the host media port has no bounded recording operation. The UI says
so and leaves import and screenshots available.

It uses no third-party dependencies.
