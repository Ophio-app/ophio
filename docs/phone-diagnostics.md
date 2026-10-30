# Phone diagnostics

`client/src/diagnostics/reports.ts` exposes `startReport(origin, context)`,
`toggleExtra`, `setWords`, `setScreenshot`, and `send` for the report sheet.
A draft contains its UUIDv7 id, six-character reference, local salt, the
redacted bundle, and `description` for See all. Render that description after
each edit. `send` saves the reviewed bundle before returning its reference;
it throws if that durable save fails. Delivery and retries run in the
background. The `reports` store holds queued bundles and directly sent report
summaries. Its `error` is a local delivery problem, never an intake message.

The service requests a computer preview and sends report commands only when
that computer advertises `reports_v1`. A failed preview leaves a phone-only
draft. Submission first tries the reachable original computer when its
sending setting allows it, then direct intake, then the durable queue. Retries
keep the same report id and reviewed payload. They never add fresh diagnostics
after review. Status checks for direct submissions run every five minutes
while visible; delivery retries run every minute and on reconnection.

The optional local `context.errorSide` selects the signature's build; it is
not a wire field. A command error defaults to the computer, other errors to
the phone. Crash-origin reports use the captured crash's code and build.

Set `VITE_OPHIO_INTAKE_URL` at build time to enable direct intake. HTTPS is
required except for loopback test servers. Direct intake requires Ed25519
WebCrypto and IndexedDB. Its per-install private key is generated
non-extractable and stored as a CryptoKey, never serialized or exported.
No crypto fallback is used. HTTP requests omit cookies, reject redirects,
and sign the method, actual request path, timestamp and exact body hash.
An empty URL disables the direct path.

The trail keeps at most 400 events from the last ten minutes. It stores no
message bodies or route arguments. It flushes at most once a second and on
page hide or backgrounding. Crash records are scrubbed and saved immediately.
`crashes.pending()` supports the next-start offer. `addCrashSource(fn)` can
register a native `takeNativeCrashes` provider; sources return already
structured `CrashRecord[]`. Captured crashes are acknowledged only after the
report has been saved to the local delivery queue.

Installing an update stops the app, so the next start is no time to ask
about a crash. The native provider also says when the app was installed or
last updated (Android's `lastUpdateTime`), and `crashes.forgetBefore(at)`
drops everything recorded before then, kept or still to come from the phone.
The offer says "Ophio closed unexpectedly" only for an exit or a panic; a
script or render error left the app running, so it says "Ophio ran into a
problem".

Phone versions use the existing app-info build and protocol, plus the OS
version when the WebView reports it. The model is empty because the current
app-info API exposes a device name that may be personal. A link defaults to
direct; transports that can identify relay connections can set the optional
`route: "relay"` on the existing online link state.

State redaction preserves numeric and boolean values, structure, known wire
enums, logical screen names and recognized model names. Unknown strings
become kind-and-length markers. Identifiers and dictionary identities,
including dynamic keys, become report-scoped pseudonyms. Regenerate the
string allowlists after protocol or interface-store changes:

```sh
cd client
node scripts/protocol-barrel.mjs
node scripts/report-state-schema.mjs
```

To open a saved state in the simulator, put a bundle or state-shape JSON file
in `client/src/dev/sim/reports/example.json`, run `npm run dev:sim`, and open
`?report=example`. The loader waits for the first simulated snapshot and then
replaces the host and interface stores. It is behind the static sim-mode
import gate and is excluded from production bundles. Text markers stand in
for private content; simulated commands still use the authored scenario.

Bundle trimming always removes state first, then the oldest event across
both trails, then the screenshot, extras (oldest transcript entries first),
and oldest crashes. It preserves the person's words. An oversized required
portion fails locally instead of sending an invalid report. See all is a
lossless traversal of the final JSON, including empty containers.

The report UI is in `client/src/reports/`. It keeps consume-once native
crashes in the registry and the prompt until sent or dismissed. Sent bundles
remain on this phone for See what was sent, with a local task/computer link
for consented follow-ups. These local links never enter a report bundle.
The phone’s sending choice also applies to its direct queue. Withdrawing
waits for any active delivery to settle before deciding which route owns it.

The simulator supports preview, submission, questions, withdrawal and a
simulated app update. `?report-phone` enables screenshot detection drills;
`?report-picture` enables a made-up own-view capture; `?report-crash` offers
a computer crash on startup. `window.__sim.reports.screenshot()` exercises
the chip and `window.__sim.host.reports.crash()` offers a fresh crash.
Run `node scripts/report-ui-check.mjs <sim-url> <screens-directory>` for the
phone journey. Set `OPHIO_CHECK_GPU` to an expected renderer name when checking
a GPU run; the script stays headless in either mode.

A fix reuses AppUpdate when an offered app version matches its fixed build.
AppOffer currently carries a version, not the release feed’s build identity;
other fixes show computer update instructions. Computer update and rollback
have no client command. A conversation follow-up also needs this phone’s
saved link to the original task; it never guesses from a pseudonym.
