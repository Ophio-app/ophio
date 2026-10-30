# Dependency and identity inventory

All product-owned source is Apache-2.0.
Versions below are the resolved versions in this workspace’s Cargo.lock;
constraints remain explicit in each owning Cargo.toml.

| Rust dependency | Locked version | Declared license |
| --- | --- | --- |
| async-trait | 0.1.92 | MIT OR Apache-2.0 |
| base64 | 0.22.1 | MIT OR Apache-2.0 |
| bytes | 1.12.1 | MIT |
| ed25519-dalek | 2.2.0 | BSD-3-Clause |
| futures-util | 0.3.34 | MIT OR Apache-2.0 |
| http-body-util | 0.1.5 | MIT |
| hyper | 1.11.1 | MIT |
| hyper-util | 0.1.21 | MIT |
| rcgen | 0.14.10 | MIT OR Apache-2.0 |
| ring | 0.17.14 | Apache-2.0 AND ISC |
| rustls | 0.23.45 | Apache-2.0 OR ISC OR MIT |
| serde | 1.0.229 | MIT OR Apache-2.0 |
| serde_json | 1.0.151 | MIT OR Apache-2.0 |
| tauri | 2.12.0 | Apache-2.0 OR MIT |
| tauri-build | 2.7.0 | Apache-2.0 OR MIT |
| tauri-plugin | 2.7.0 | Apache-2.0 OR MIT |
| tauri-plugin-opener | 2.6.0 | Apache-2.0 OR MIT |
| tempfile | 3.27.0 | MIT OR Apache-2.0 |
| thiserror | 2.0.21 | MIT OR Apache-2.0 |
| tokio | 1.53.1 | MIT |
| tokio-rustls | 0.26.5 | MIT OR Apache-2.0 |
| tokio-tungstenite | 0.29.0 | MIT |
| tokio-util | 0.7.19 | MIT |
| tracing | 0.1.44 | MIT |
| url | 2.5.8 | MIT OR Apache-2.0 |
| zeroize | 1.9.0 | Apache-2.0 OR MIT |

`rcgen` and `tempfile` support host tests. `tauri-build` and `tauri-plugin` are
build tools. Workspace path dependencies retain their existing Apache-2.0
licenses. The development host includes the real `companion`, `services`,
`store` and `media` crates and never ships in the Android APK. Host-only Tauri
checks link the machine’s existing GTK/WebKit libraries (including LGPL
components); those system libraries are not included in either Android APK.

Android runtime dependencies: AndroidX core-ktx 1.16.0, appcompat 1.7.1,
activity-ktx 1.10.1, webkit 1.14.0, lifecycle-process 2.10.0, Material 1.12.0,
and ZXing core 3.5.3: Apache-2.0. Android Gradle Plugin 9.3.1 and the Gradle
wrapper are Apache-2.0 build tools; Kotlin is Apache-2.0. Keystore instrumentation
uses Android’s built-in Instrumentation API without an external test runner.
ktfmt 0.54 is an Apache-2.0 formatting tool downloaded only into ignored target/.
No Play Services, remote barcode model, push, analytics, or inference SDK ships.

The official Tauri opener plugin 2.6.0 supports Android and is MIT/Apache-2.0.
The official barcode plugin was evaluated but not included: its Android path
uses Google Play Services MLKit. Camera2 plus ZXing keeps QR decoding local.
Haptics use Android’s existing view feedback and need no additional permission.

App identity is centralized in tauri.conf.json: display label `Øphio` and
application id `app.ophio`. tools/sync-native.mjs updates Android labels,
namespace, application id and MainActivity package before a build. It also
generates native colors from the shared CSS tokens and verifies that the client
pairing scheme matches the computer's `PAIR_SCHEME`. The scheme is `ophio`
(defined by the client brand module and `ports::PROGRAM_NAME`). No intent
filter is registered for pairing links; QR and paste consume that URI.

Other technical identifiers: plugin Java namespace `app.ophio.device`; internal
Rust packages `phone-shell`, `tauri-plugin-device`, and `dev-host`, which keep
their working names; library `phone_shell`; Android theme
`Theme.phone_shell`; FileProvider authority `${applicationId}.fileprovider`;
notification channels `attention` and `connection`; Keystore alias prefix
`device.`. App-private data lives under Android’s application-id directory;
there is no separate branded config-directory constant.

License sources: the locked crates’ distributed Cargo.toml/license files,
Android Maven artifacts, [Tauri plugins](https://github.com/tauri-apps/plugins-workspace),
[ZXing](https://github.com/zxing/zxing/blob/master/LICENSE), and
[ktfmt](https://github.com/facebook/ktfmt/blob/main/LICENSE).
