<!-- SPDX-License-Identifier: Apache-2.0 -->
# Build Øphio for iPhone

Ophio for iPhone is not on the App Store or TestFlight yet. We are awaiting
Apple's developer-account approval. This guide will gain a TestFlight link
once approval makes that distribution available.

For now, developers can build the app on a Mac, run it in Apple's simulator,
or try installing it on their own iPhone with their own free Apple ID and
Personal Team. These are developer builds, not a supported iPhone release.
Only simulator runs have been tested; no installation or signing on a real
iPhone has been attempted. The real-device steps below are not verified.

The app requires iOS 16 or later and a separately installed Øphio computer
companion. Building the phone app does not install or start the companion.

## What you need

- A Mac supported by **Xcode 27.2**, with its **iOS 27.2 SDK and simulator
  runtime**. These are the versions required for the commands below and used
  for simulator checks; earlier Xcode versions are not qualified. The app's
  deployment minimum remains iOS 16. Install full Xcode, not just its Command
  Line Tools. Launch it once to accept its licence and finish setup. In Xcode's
  settings, select its Command Line Tools and install the simulator runtime.
- Rust and Cargo **1.92 or newer**, installed through
  [rustup](https://rustup.rs/).
- Node.js **22.22.2 or newer within 22**, **24.15 or newer within 24**, or
  **26 or newer**, with npm supporting lockfile version 3. Node.js 23 and 25
  are not supported. See [BUILDING.md](../../BUILDING.md).
- [Homebrew](https://brew.sh/), with XcodeGen and CocoaPods:

  ```sh
  brew install xcodegen cocoapods
  ```

Tauri **2.12.0** is installed by `npm ci` from the project's lockfile. Use
`npm run tauri` below; a separately installed global Tauri command may differ.
No Android SDK, App Store Connect API key, paid developer membership, or
signing certificate is needed for the simulator.

Check the tools in Terminal:

```sh
xcodebuild -version
xcode-select -p
rustc --version
cargo --version
node --version
npm --version
xcodegen --version
pod --version
```

Use a UTF-8 terminal locale so CocoaPods can read its inputs correctly. For
example, in the same Terminal session:

```sh
export LANG=en_US.UTF-8
export LC_ALL=en_US.UTF-8
```

## Prepare the source and your own app identity

These instructions describe the 0.3.0 source. Use its source checkout or
release source archive; an older published version may not contain this
workflow. The public repository is
[ophio-app/ophio](https://github.com/ophio-app/ophio).
Start in the source tree's top-level folder, which contains `Cargo.toml`,
`client/`, and `packaging/`.

Keep build output and your local configuration outside the checkout:

```sh
unset APPLE_DEVELOPMENT_TEAM
export OPHIO_IOS_BUILD="$(mktemp -d)"
export CARGO_TARGET_DIR="$OPHIO_IOS_BUILD/target"
export CARGO_BUILD_JOBS=2
export VITE_OPHIO_DOWNLOAD_URL="https://ophio.app"
```

`VITE_OPHIO_DOWNLOAD_URL` sets the companion's public download page in the
phone app. It is not an iPhone distribution link. Without this build setting,
the pairing page asks you to get an archive from the person who supplied the
app instead of offering the public download page.

Clearing `APPLE_DEVELOPMENT_TEAM` in this Terminal session prevents a team left
from another project from overriding the local JSON configuration below.
Choose your own Personal Team there when following the real-device steps.

Every developer must use their **own bundle identifier** instead of the
project's `app.ophio`. Replace `com.example.ophio.dev` below with your own
unique reverse-domain identifier before running it. Keep the same identifier
for later builds if you want to keep that app's saved pairings and data.

```sh
cat > "$OPHIO_IOS_BUILD/tauri.local.json" <<'JSON'
{
  "identifier": "com.example.ophio.dev"
}
JSON
```

This local file overrides the identifier without editing shared source.
Pass it to **both initialization and every subsequent build**. Keep the
iOS product name `Ophio` unchanged: the on-screen name is Øphio, but Apple's
bundle lookup needs the plain file name.

On an Apple silicon Mac, install the simulator and physical-device Rust targets:

```sh
rustup target add aarch64-apple-ios-sim aarch64-apple-ios
```

On an Intel Mac, use `x86_64-apple-ios` instead of
`aarch64-apple-ios-sim`. Intel simulator builds are not verified.

Install the locked JavaScript dependencies and generate the Xcode project:

```sh
cd client
npm ci --no-audit --no-fund
npm run tauri ios init -- --ci --config "$OPHIO_IOS_BUILD/tauri.local.json"
npm run tauri icon -- src-tauri/icons/icon.png
```

The last command must run **after** initialization. A clean initialization
creates Tauri's default icon catalog; the icon command replaces that generated
catalog's artwork with Øphio's. It also regenerates desktop and Android icon
files locally. Do not commit those generated changes just to make a personal
build. Keep the tracked Android project: the shared native-resource build
step uses it even when the target is iPhone.

The Xcode project under `client/src-tauri/gen/apple/` is generated and ignored
by Git. Do not commit it or your team/account settings. If you change your
local identifier or team later, repeat initialization and the icon command
with the updated local configuration before building.

## Build and run in the simulator

From `client/` on an Apple silicon Mac:

```sh
npm run tauri ios build -- --debug --target aarch64-sim \
  --config "$OPHIO_IOS_BUILD/tauri.local.json"
```

On Intel, replace `aarch64-sim` with `x86_64`. This builds the actual native
phone shell and packaged web interface, not the browser's simulated app.
It does not require an Apple ID or Personal Team.

Open Simulator, select an available iPhone, and wait for it to finish starting.
Then use the app path printed by the build, or locate it and install it:

```sh
open -a Simulator
xcrun simctl list devices booted
APP_PATH="$(find src-tauri/gen/apple -type d -name Ophio.app -print | head -n 1)"
test -n "$APP_PATH"
xcrun simctl install booted "$APP_PATH"
xcrun simctl launch booted com.example.ophio.dev
```

Replace the last command's identifier with the same one in your local JSON.
Have only the simulator you want to use booted. A simulator cannot validate
real camera scanning, Face ID hardware, physical-device provisioning,
network-permission behavior, or microphone interruptions by phone calls.

## Try your own iPhone with a free Apple ID

**Not verified on a real iPhone.** Apple's free Personal Team lets you develop
on your own devices without buying Developer Program membership. It does not
permit TestFlight or App Store distribution. Free-team installs expire after
**seven days**; rebuild and reinstall to continue using the app. Apple's free
account limits and supported capabilities also apply.

1. In Xcode's account settings, sign in with **your own** Apple ID. Select
   your Personal Team; do not use another developer's team or bundle identifier.
2. Find that team's identifier in Xcode and add it to your local configuration.
   Keep your own identifier from above and replace `YOUR_TEAM_ID`:

   ```json
   {
     "identifier": "com.example.ophio.dev",
     "bundle": {
       "iOS": {
         "developmentTeam": "YOUR_TEAM_ID"
       }
     }
   }
   ```

3. Connect and unlock your iPhone, trust the Mac when asked, and enable
   **Settings → Privacy & Security → Developer Mode** if iOS asks for it.
   This can require restarting the phone.
4. From `client/`, regenerate with that same configuration and open the build
   in Xcode:

   ```sh
   npm run tauri ios init -- --ci --config "$OPHIO_IOS_BUILD/tauri.local.json"
   npm run tauri icon -- src-tauri/icons/icon.png
   npm run tauri ios build -- --debug --open \
     --config "$OPHIO_IOS_BUILD/tauri.local.json"
   ```

   `--open` opens Xcode **before** building a signed archive. Leave the Terminal
   command running while using this Xcode build session.
5. In Xcode, select the iOS application target. Under **Signing & Capabilities**,
   enable **Automatically manage signing**, select your Personal Team, and
   confirm that the bundle identifier is yours. Select your connected iPhone
   as the run destination, then choose **Product → Run**. Let Xcode provision
   and install the development build. If iOS asks you to trust its developer,
   follow the prompt under **Settings → General → VPN & Device Management**.

If signing fails, read Xcode's account or provisioning error. Do not switch to
the project's identifier, another team, or an App Store export method. No
project-owned signing credentials are supplied by this guide.

To check device compilation **without signing or installing**, use this instead:

```sh
npm run tauri ios build -- --debug --target aarch64 --no-sign --archive-only \
  --config "$OPHIO_IOS_BUILD/tauri.local.json"
```

An unsigned archive is not an installable iPhone app and proves nothing about
Personal Team provisioning or real-device behavior.

## Pair with your computer

Install the **released companion separately**, using the [main installation
instructions](../../README.md#get-started) and the guide for
[Linux](../linux/INSTALL.md), [Mac](../macos/INSTALL.md), or
[Windows](../windows/INSTALL.md). Use matching phone/companion versions where
available; a development phone build does not upgrade the companion.

Run this on the computer where the companion is installed:

```sh
ophio pair
```

In the phone app, choose **Pair a computer**. Scan the code with a real camera,
or paste the pairing link into the app; pasting is also the simulator route.
Confirm at the computer only after checking that its six-digit confirmation
code matches the phone's. Treat the pairing link as sensitive and do not share
or commit it.

A real iPhone must be able to reach that computer over trusted Wi-Fi or
Tailscale configured on both ends. USB provisioning through Xcode does not
provide that connection. Complete the companion's network setup and allow
local-network access on the iPhone when prompted. `localhost` on a real phone
means the phone itself, not your Mac or the companion computer.

## What this does not establish

Only simulator behavior has been tested. Real-iPhone installation, free-team
signing, camera pairing, permission recovery, and long-running background
behavior remain unverified. Browser responsive previews and simulator screen
sizes are also **not native iPhone Duo folding or hinge tests**; no such
proof is claimed. iPhone support remains experimental, with the platform
limitations described in the [native-shell guide](../../client/src-tauri/README.md#iphone).

Remove your temporary build folder once you no longer need its outputs and
local configuration. Keep the identifier you chose available for your next
build, but never commit personal account settings.
