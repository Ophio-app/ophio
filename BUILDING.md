# Building and packaging

Product-owned code is Apache-2.0. The display name comes from
`client/src/brand.ts`; release scripts derive their archive names from it.
`Cargo.toml`'s workspace version is authoritative. Packaging rejects a different
version in the client package, native client manifest, or Tauri configuration.
After changing the display name, regenerate NOTICE with
`python3 scripts/package-artifacts.py notice`.

## Prerequisites

- Rust/Cargo 1.92 or newer, edition 2024; current builds use 1.98.
- Python 3.11 or newer; packaging and licence scripts use only its standard library.
- Node.js 22.22.2 or newer within 22, 24.15 or newer within 24, or 26 or newer,
  the range every locked client and test dependency supports (Node.js 23 and
  25 are excluded; Ubuntu 24.04's packaged Node.js 18 is too old), and npm with
  lockfile v3 support.
- Linux x86_64: C/C++ compiler, pkg-config, libclang, and the PipeWire and
  xkbcommon development files. Use your distribution's packages; the build
  scripts never install system dependencies. On Ubuntu 24.04:
  `sudo apt install build-essential pkg-config libclang-dev libpipewire-0.3-dev libxkbcommon-dev bubblewrap`.
- Browser tests: Playwright's Chromium and its system libraries, which
  `npm test` and `npm run smoke` use, and a Chromium-family browser at
  `/usr/bin/chromium`, `/usr/bin/chromium-browser`, `/usr/bin/google-chrome`,
  `/usr/bin/microsoft-edge` or `/usr/bin/brave-browser` for `cargo test`. The
  first commands under "Build and test" install the client packages and
  Playwright's Chromium, and can link that Chromium as `/usr/bin/chromium`.
- Linux Chat access controls: bubblewrap 0.9 or newer at `/usr/bin/bwrap`, with user,
  PID and network namespace creation allowed in the companion’s environment.
  Build `ophio-sandbox-shim` (package `sandbox`) and keep it executable beside
  `ophio`, in the same build profile. See the
  [Linux setup and troubleshooting guide](packaging/linux/INSTALL.md#chat-access-controls)
  for the namespace probe, AppArmor requirements and missing-dependency remedies,
  and the [sandbox guide](crates/sandbox/README.md) for isolation details.
- Android: JDK 21, Android SDK platform/build-tools 37, NDK 28.2.13676358
  (SDK manager packages `platforms;android-37.0`, `build-tools;37.0.0` and
  `ndk;28.2.13676358`), the `aarch64-linux-android` Rust target, and the locked
  Gradle wrapper. The
  project uses Gradle 9.6.1, Android Gradle Plugin 9.3.1 and Kotlin 2.2.10.
- Windows cross-build: the `x86_64-pc-windows-gnullvm` Rust target and LLVM/MinGW.
  Configure Cargo's target linker and C/C++ compiler variables for that target.
  The current runtime-notice baseline is LLVM 23.1.2. Do not silently substitute
  a different static runtime without updating its inventory and notices.
- Advisory checks (`scripts/ci.sh advisories`): cargo-audit 0.22.2 and network
  access. The stage finds `cargo-audit` on `PATH`, or else under
  `$BUILD_TOOLS_HOME/cargo-audit/bin` when `BUILD_TOOLS_HOME` is set. Install it
  into the build tools directory, not the user's Cargo home:
  `cargo install --locked --version 0.22.2 --root "$BUILD_TOOLS_HOME/cargo-audit" cargo-audit`.
  The stage audits both Cargo lock files and `npm audit --omit=dev` in `client/`,
  denying warnings, and fails when the tool is missing or a fetch fails, even
  with `CI_ALLOW_MISSING_TOOLS=1`. Each ignored advisory, with its reason, lives
  in `.cargo/audit.toml` for the root lock file and
  `client/src-tauri/.cargo/audit.toml` for the Tauri lock file. The hosted
  Checks workflow runs this stage as its own `advisories` job, apart from the
  build, so a newly published advisory fails that job by name.

Use a build-specific toolchain environment. The scripts accept
`BUILD_ENV_FILE=/path/to/build-env.sh`, or an environment sourced before invoking
them. It must supply `JAVA_HOME`, `ANDROID_HOME`, `NDK_HOME`, target linkers and
any dedicated `CARGO_HOME`/`RUSTUP_HOME`/`GRADLE_USER_HOME`/npm cache directories.
No script modifies shell startup files or global tool configuration.

### Building from a source archive

Work in a Git checkout. Some tests read the list of files Git tracks, Android
packaging copies exactly those files, and packaging takes its timestamp from
the last commit unless `SOURCE_DATE_EPOCH` is set. A source archive without
history (such as GitHub's "Source code" download) must first be committed into
a new repository, before the commands below. The commit needs only a
placeholder identity:

```sh
git init -q && git add -A
git -c user.name="Local build" -c user.email=build@example.invalid commit -qm "Source archive"
```

### Build and test

Install the client packages and Playwright's Chromium first: the Rust browser
tests and the link below need them. Run the `ln` line only when none of the
browsers listed under Prerequisites is installed.

```sh
npm --prefix client ci --ignore-scripts --no-audit --no-fund
(cd client && npx playwright install --with-deps chromium)
sudo ln -s "$(cd client && node -p 'require("playwright").chromium.executablePath()')" /usr/bin/chromium
export CARGO_TARGET_DIR="$PWD/target" CARGO_BUILD_JOBS=8
cargo build --locked -p companion -p computer-mcp -p sandbox
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
env -u DISPLAY -u WAYLAND_DISPLAY -u DBUS_SESSION_BUS_ADDRESS cargo test --workspace
npm --prefix client run typecheck
npm --prefix client test
npm --prefix client run build
npm --prefix client run smoke
python3 -m unittest discover -s scripts/tests -v
```

The normal tests use fakes, isolated files and loopback; desktop adapter tests
must run with display/session variables unset. Never enable desktop capture or
run real agent inference merely to verify a build. Native Android and Windows
runtime qualification are separate from compilation.

Shell screen tests use real lazy routes with simulated connection timers.
Advancing fake time does not finish a dynamic import. Preload the screens a
suite visits in `beforeAll`, before installing fake timers, and wait for
`vi.dynamicImportSettled()` inside `act` after navigation. Then advance the
simulated replies in a second `act`. Wait for the session's pending queries
to settle and the visible route to leave its loading state before reading its
text; an empty route must not pass a negative assertion. Advance only as much
simulated time as readiness needs, so unrelated background activity does not
fill the test's budget. Restore real timers in `afterEach` even when cleanup fails.
The permission suite leaves the normal test timeout in place and gives only
its cold screen imports a separate setup budget. Contribution tests also wait
for observable receiver replies because SHA-256 uses real asynchronous work.
Run these checks with `npm --prefix client test --
src/shell/permissions.test.tsx src/screens/contribute/contribution.test.tsx`;
include the full client suite when checking changes to their synchronization.
A permission case must also pass with a cold module cache: add
`-t 'without terminal,'` to the permission-file command to check it without
earlier cases warming its routes.

Run the upload continuation tests with
`npm --prefix client test -- src/host/transfer.test.ts src/screens/transfers`.
The transfer fixture retains uploaded bytes and reports their offset; the real
upload loop must reuse the existing transfer, send only the remaining bytes
(including a short final chunk), and preserve the complete file and SHA-256.
Cases resume both at a chunk boundary and 17 bytes past it. To check that this
coverage catches an ignored offset, temporarily replace `Number(start.offset)`
with `0` in the upload loop in `client/src/host/transfer.ts`: both upload resume
cases must fail with “The computer received different bytes.” Restore the loop
afterward. These are client protocol checks against a fake computer; they do
not qualify native phone reconnect behavior.

Run the native clip integrity tests with
`npm --prefix client test -- src/screens/annotate/nativeTrim.test.ts`. They use
Node's real Blob implementation so SHA-256 checks read the assembled bytes.
The bridge fake returns distinct chunks at the requested offsets, including a
short final chunk. The tests check the exact output and reject changed,
zero-filled, duplicated and reordered bytes against the original metadata hash,
with staging cleanup on rejection. These are client assembly checks; they do
not exercise the native video remuxer on a phone.

`npm run smoke` is the browser check. It needs Playwright's Chromium (installed
under "Build and test") and starts its own simulated
computer on a free loopback port. It walks the main journeys — pairing, a new
chat, its permissions, approving a step, turning a chat into a task, Next up,
saving a Library file offline and through a conflict, and recovering an
interrupted task — in portrait, landscape and at 200 % text. Each step names a
control by its role and label and must reach it; each journey asserts where it
ends. A console or page error, or content wider than the screen, fails the
journey. First it runs `client/scripts/smoke/canary.json`, whose shots each
break one of those rules, and stops unless every one fails. The exit status is
non-zero when anything failed; screenshots and logs go to a temporary folder,
or the folder given after `--`. The journeys are in
`client/scripts/smoke/plan.json`. `scripts/shots.mjs` without `--strict` still
takes exploratory screenshots from the plans in `client/scripts/plans/`; those
only report problems.

## Offline preparation

Both Cargo workspaces have independent lockfiles. Populate the selected Cargo
registry cache before disconnecting:

```sh
cargo fetch --locked --target x86_64-unknown-linux-gnu
cargo fetch --locked --target x86_64-pc-windows-gnullvm
cargo fetch --locked --manifest-path client/src-tauri/Cargo.toml --target aarch64-linux-android
npm --prefix client ci --ignore-scripts --no-audit --no-fund
```

Android additionally needs the Gradle distribution, plugins, runtime dependency
POMs/JARs/AARs, SDK and NDK already cached. Resolve those with the standard Tauri
Android build in your own prepared build environment before requiring offline
operation, for example in a separate clone with the same environment:
`npm --prefix client ci --ignore-scripts && (cd client && npx tauri android build --ci --target aarch64 --apk --aab)`.
Packaging forces Cargo, npm and Gradle offline and reports missing
cache entries. It never fetches speech models. Once cached, add `--offline` to
Cargo commands and `npm ci`; no registry credentials are needed for these public
dependencies.

Gradle checks its distribution against the wrapper's `distributionSha256Sum`
(Gradle's published value from https://gradle.org/release-checksums/) and every
plugin, POM and library against SHA-256 entries in
`client/src-tauri/gen/android/gradle/verification-metadata.xml`, and
`scripts/tests/test_android_verification.py` pins `gradle-wrapper.jar` to Gradle's
published wrapper checksum from the same page. After changing the Gradle version
or an Android dependency, update both checksums and regenerate
the metadata in `client/src-tauri/gen/android/` after one Tauri Android build.
Use an empty Gradle home, since a warm cache omits parent POMs, and review the
diff before committing.

```sh
rm gradle/verification-metadata.xml
GRADLE_USER_HOME="$(mktemp -d)" ./gradlew --write-verification-metadata sha256 \
  -x rustBuildUniversalDebug -x rustBuildUniversalRelease \
  assembleUniversalDebug assembleUniversalRelease bundleUniversalRelease \
  :tauri-plugin-device:assembleDebugAndroidTest :tauri-plugin-device:lintDebug
```

## Local release candidates

Run one command from a clean Git checkout (see "Building from a source archive"
above) with prerequisites and dependency caches prepared. Packaging takes its
timestamp from the last commit unless `SOURCE_DATE_EPOCH` is set, and Android
packaging copies the files Git tracks. Android packaging also needs
`npm --prefix client ci --ignore-scripts` in that checkout.

| Platform | Command | Successful output |
| --- | --- | --- |
| Linux | `scripts/package-linux.sh` | `dist/linux/<name>-<version>-x86_64-linux.tar.gz` |
| Windows, cross-build only | `scripts/package-windows.sh` | `dist/windows/<name>-<version>-x86_64-windows.zip` |
| Android ARM64 | `scripts/package-android.sh` | unsigned APK/AAB and separate locally test-signed APK under `dist/android/` |

All scripts generate and check licences before final distribution assembly.
The archives contain both companion binaries, LICENSE, NOTICE,
THIRD-PARTY-NOTICES.txt, sbom.cdx.json, the licence review report, and platform
installation instructions. Linux additionally includes `install.sh`, which runs
the archive's `ophio install` (see `packaging/linux/INSTALL.md`), and verifies
the extracted companion's `--version` and `--help` using isolated application
directories. It never installs or starts a service; `ophio service install`
writes the user unit when someone runs it.
Windows checks x64 PE headers; no Windows runtime claim follows from that check.

`dist/MANIFEST.txt` lists every distribution file, byte count and SHA-256. Archive
entry order, timestamps and modes are stable for identical inputs;
`SOURCE_DATE_EPOCH` defaults to the current commit timestamp. This does not claim
bit-identical Rust/Gradle rebuilds across different toolchains or source paths.
The temporary Android test certificate changes on every packaging invocation.
Release builds remap the checkout, the toolchain caches and the Cargo target
folder (wherever it is) in Rust and native C/C++ diagnostics, and Android's
staged copy of the checkout reads as the checkout itself, so packaged
executables do not disclose the builder's local paths. Still run
`scripts/artifact_scan.py` over each built file before release.

Android builds in `target/packaging/android/project/`, copied from tracked
checkout inputs. Tauri's generated native files never modify the checkout.
Its first build resolves the native project, then an offline Gradle report records
`universalReleaseRuntimeClasspath`, including AndroidX, Kotlin and plugin runtime
dependencies. The second build embeds verified licence assets. Generated assets
are available at `client/dist/licenses/` in that staged build and as APK entries
`assets/licenses/{LICENSE,NOTICE,THIRD-PARTY-NOTICES.txt,sbom.cdx.json}`.
Settings → Versions → Open-source notices shows the bundled `NOTICE` and
`THIRD-PARTY-NOTICES.txt` from `/licenses/` through Tauri's embedded frontend; a
development build without them says so. The script verifies all four packaged
native asset bytes.

The locally test-signed APK contains the release build, signed with a fresh
throwaway Android Debug certificate, not a production signing identity. The
keystore and password exist only in a temporary build-output directory and are
deleted after signing. `apksigner verify --verbose --print-certs` and
`aapt2 dump badging` validate the test copy; identity, version and ARM64 ABI are
checked against the source configuration. No emulator or physical device is
started or installed to by packaging.

Public signing uses a separate owner-operated step, never a build-time key.
See the [protected signing ceremony and offline helper](packaging/android/SIGNING.md)
for the account boundary, off-machine backup, exact command and unfinished
Android rotation. The checked-in public certificate policy names the public
release key, which every build's companion trusts; a personal build for phones
that still run the earlier development key sets `OPHIO_PERSONAL_SIGNING=1` to
trust that one instead. `OPHIO_PUBLIC_RELEASE=1` makes packaging fail closed
unless the policy and the companion's public pin agree and the personal setting
is off; Android public
preparation produces unsigned artifacts only, not a test-signed fallback. It
requires empty `dist` (an existing empty `dist/android-public-unsigned/` is allowed)
and rejects symlinked output directories; use a fresh throwaway checkout rather
than deleting previous artifacts. To embed a signed `OPHIO_PHONE_APK` in a public
Linux or Windows archive, also set `OPHIO_ANDROID_BUILD_TOOLS` to an absolute SDK
build-tools directory, for example `/trusted/android/build-tools/37.0.0`. Packaging
requires working `apksigner` and `zipalign`, verifies a private copy's signature,
v3 support, pinned signers and alignment, then archives those exact bytes.
It never falls back to test signing or another tool location.
Ordinary local packaging retains its existing certificate checks and needs no
additional Android tools when the public variable is unset or `0`.

The fresh-key helper verifies the approved unsigned input hash and resulting
v2/v3 certificate before returning a signed APK. It neither proves that the key
is protected nor authorizes publication. Rotated personal-install upgrades,
AAB/upload-key signing, live key setup and real device migration remain separate,
owner-approved work; no production signing material is used by packaging tests.

## Licence inventory and release checks

```sh
python3 scripts/licenses.py --check
scripts/source-candidate.sh --json target/packaging/source-candidate.json
python3 scripts/earlier_names.py
```

The offline generator reads locked Cargo metadata and uses Cargo's target-specific
normal/build dependency tree for the two host binaries or native phone shell.
It excludes development edges and test-only crates. Build inputs are deliberately
included even when the linker discards them. For the web client it conservatively
includes the npm production dependency closure, including peers; it does not
claim an exact Rollup/Rolldown tree-shaken module inventory. Bundled font files
are identified by their hashes when the Android script supplies the built web
directory. Standalone checks without it inventory all production font files.
Icons, native runtime libraries, embedded SQLite/IJG material and pinned speech
model downloads have separate entries. Models are labelled external runtime
components and are never shipped as weight files.

Cargo hashes identify registry archives; npm integrity hashes identify package
archives. Maven artifacts and font files have computed SHA-256 values. Model
hashes are read from the speech catalog. Artifact hashes are in the distribution
manifest. Missing Gradle reports, licence texts or unrecognized licence terms
fail the check; an incomplete SBOM is explicitly marked incomplete and is never
silently promoted into an archive. MPL/LGPL obligations appear as review items.
The generator retains all supplied licence, copyright and NOTICE files, even
when the selected licence expression permits a simpler alternative.

`packaging/license-policy.toml` holds policy and any exact, reasoned exceptions.
`packaging/license-evidence.toml` records hash-pinned upstream texts omitted from
published crate archives, fetched at their Cargo VCS revisions. These are
supplemental evidence, not policy waivers. Static runtime inventory changes need
review whenever a compiler, codec feature or platform target changes.

The source checker examines **`git archive HEAD`**, not uncommitted edits or the
repository history. It reports every forbidden path/content location and missing
owned-source SPDX header without echoing potentially private line contents. That
includes home directories on Linux, macOS and Windows, files named after work
items (such as `U32.json`) and notes on how the product was built. When the Git
directory holds an untracked `info/private-terms` file (one term per line, `#`
for comments), each term found in a path or line is reported by rule and
location, never by the term itself, and paths are shown with the term masked.
Only marked generated bindings have a documented header exception. Fix and
commit all findings, then rerun it. A passing scan is a bounded mechanical check,
not a substitute for reviewing a proposed public export for confidential text.

`scripts/earlier_names.py` fails when a tracked file uses the identity the
product had before it was named: its Android namespace, link schemes,
variables, Library file prefix, directories, commands or display name. Its
`ALLOWED` list names the files that carry an earlier install across, each with
its reason, and fails when one of them no longer needs the exception. The CI
`source` stage runs it.

Build a release from a clean tree after these checks and a review of its
content.

Linux release executables use **glibc 2.39** as their maximum ABI requirement
(Ubuntu 24.04). A build on a newer host is not a portable release: packaging
checks the version requirements of `ophio`, `computer-mcp`, and `ophio-sandbox-shim` with
`readelf` and rejects anything above that ceiling.

The disposable Ubuntu 24.04 lab guest can build offline from committed source
and vendored dependencies using `tools/lab/guest/release.sh`. It reuses the
installed Rust 1.98 toolchain and native libraries, writes only to the lab user's
build directories, and installs no packages. This baseline was chosen because
the existing guest provides a reproducible older userspace without another image
or hosted build service. Extracted release archives must also be exercised there;
a successful build alone is not runtime qualification.
