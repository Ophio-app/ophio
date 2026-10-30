# Protected Android signing

Ophio's public Android releases are signed with a fresh key made for them on
25 October 2026. Its certificate's SHA-256 is
`2df838f528a3edf7e6450eeac2b584e86820640876448cb5e2844c1cdd4b9516`, which
`public-signing.json` names and the companion trusts. The key is held on a
hardware security key, used only from a separate signing account, so every
signing needs the owner's password and a touch of the hardware key. It is
backed up encrypted off the machine.

Personal builds made before then were signed with a development key, and the
phones that run them keep it: such builds opt in with `OPHIO_PERSONAL_SIGNING=1`
(see below). No account, key, backup, device or installed app is changed by
these scripts, and a signed APK is not permission to publish.

## The security boundary

Use a separate Linux signing account, unlocked by the owner for each signing
session. A different directory, file mode, username or certificate fingerprint
alone does **not** keep a key beyond agents' reach.

The agent account must have no passwordless route to root or the signing account.
That includes sudo/doas rules, cached privilege, privileged groups, writable
services or privileged executables, containers with host-root access and another
credential that bypasses the owner. If any such route remains, a same-host account
is not a security boundary: do not generate the public key there. An owner must
resolve those privileges or use the separate signing account on a clean offline
Linux installation instead. These scripts do not audit or change host privileges.

The signer must not run code, shell startup files, Java, Python, Android SDK tools,
helper scripts or configuration writable by the agent account. Use trusted
system-owned tools and an owner-reviewed copy of the helper, its dependency
`release_common.py`, and public policy in signer-owned storage. Do not run the
helper directly from an agent's checkout, SDK, home directory or build cache.
Do not run Cargo, npm, Gradle, install hooks or submitted executables as the signer.
The unsigned APK is data, but its parsers are still attack surface: keep the
trusted Android tools current and review the artifact before unlocking the key.

Use a direct owner-controlled login/session, not an agent-owned terminal, shared
terminal multiplexer or browser session. Switching virtual terminals alone does
not protect passwords from guest or remote input forwarding or same-user session
control. The protected signer session needs exclusive trusted input, no
agent/development-controlled credential capture or reattachment, and closure of
prior privileged sessions and handles before entering a secret. Do not enter the
password in chat, environment variables, command arguments, logs or an
agent-writable wrapper. The helper requires a terminal and lets `apksigner`
prompt. A terminal check is only an accidental-unattended-use guard: an agent
can create a terminal too. The certificate policy and input hash likewise
identify bytes, not proof that the host, key or artifact is safe. Local
administrator compromise defeats this design even while encrypted key storage
is locked.

## Owner-owned initial ceremony

These steps are deliberately not automated. They need the owner's authentication,
physical backup handling and approval of the irreversible identity transition.

1. Establish and independently check the account/tool boundary above. Start a
   clean owner-controlled signing session without agent processes in that account.
   Use a private directory and restrictive creation mask (`umask 077`). Keep the
   signing account free of agent-accessible remote-login credentials and do not
   leave the keystore unlocked between sessions.
2. Generate a **fresh** Android key using trusted `keytool`, with a strong unique
   password entered at its prompts. For example, choose PKCS12, RSA 4096 and a
   validity period of at least 25 years. Do not reuse or copy the personal key
   as the public identity. Decide the certificate subject and alias before
   generating it; they are public identity information, not secrets.
3. Make an encrypted off-machine backup of the keystore and required recovery
   information. Keep its password/recovery secret separately. Test restoration
   into protected temporary storage and compare the exported public certificate's
   SHA-256. Delete the restored test copy afterward. A second copy on the same
   disk is not an off-machine backup. Record successful recovery without secret
   contents or private storage locations in public records.
4. Export only the public certificate. Review its SHA-256 independently and put
   that lowercase digest in `public-signing.json`'s `certificate_sha256`. The
   existing personal fingerprint is expressly rejected. Never substitute a
   synthetic test certificate to make public checks pass.
5. Put the same digest in the companion's public pin (below) and qualify a
   clean install and a same-key update before shipping.

For Ophio this ceremony took place on 25 October 2026. The key was then moved
onto a hardware security key and the keystore file deleted, so only the
hardware key and the encrypted backup hold it. Steps 4 and 5 are this file's
policy and the companion's public pin.

## Preparing and signing one APK

Build as the ordinary unprivileged build account, without the protected key.
The existing `scripts/package-android.sh` continues to produce unsigned APK/AAB
files and a separate disposable test-signed APK. Never distribute the test copy.

`OPHIO_PUBLIC_RELEASE=1` enables public-preparation checks in packaging. It fails
if the public certificate is absent, malformed, the known personal certificate,
or different from the companion's trust pin. Android public preparation emits
**only unsigned** candidates in `dist/android-public-unsigned/`, never generates
a test key and refuses existing `dist` content, including personal artifacts or
an old `MANIFEST.txt`. The sole exception is an empty, real
`dist/android-public-unsigned/` directory. Symlinked output directories are
rejected before building. Copies are staged first; a manifest-writing failure
rolls back the new candidate. Use a fresh throwaway checkout rather than deleting
valuable existing artifacts. The top-level manifest covers all existing
distribution files, not just the most recent build. These are preparation checks,
not a public release or an attestation of account isolation. Without the variable
(or with `0`), current development/personal packaging behavior is unchanged.
A misspelled value is refused rather than treated as development mode.

After independent review, copy the unsigned APK and its approved SHA-256 into the
signer's private staging area. The owner must confirm the app identity, version,
source revision, build provenance, licence inventory and hash. A hash supplied by
the same untrusted builder is not independent approval.

The unsigned APK from public packaging is already aligned (packaging runs
`zipalign -P 16 4`), and signing does not move its entries, so it needs no
further alignment before or after signing.

Ophio's own key is on a hardware security key, so its owner signs with a signer
kept in the signing account that runs trusted `apksigner` through the hardware
key's PKCS#11 provider. It makes the same checks as the helper below: it signs
v2 and v3 without v4, and refuses a result that does not verify with v3 and
exactly the pinned certificate. For a key in a keystore file, run the reviewed
copy of the following helper from a trusted location in the signing account.
All paths here are placeholders, not machine configuration.

```sh
python3 /trusted/signing/sign_android.py \
  --input /private/staging/unsigned.apk \
  --input-sha256 APPROVED_LOWERCASE_SHA256 \
  --output /private/staging/signed.apk \
  --build-tools /trusted/android/build-tools/37.0.0 \
  --keystore /private/signing/release.p12 \
  --alias OWNER_CHOSEN_ALIAS \
  --policy /trusted/signing/public-signing.json
```

The helper copies and hashes the input, refuses an already signed APK, aligns it,
asks Android's signer to sign with v2 and v3, rejects debuggable APKs and verifies
both the cryptographic signature and every parsed signer against the approved
certificate. It never chooses a fallback key, overwrites an output, executes a
build, installs anything, starts a service or contacts a publishing endpoint.
Its temporary APKs are removed on normal success or failure. It does not create,
read into Python, back up, rotate or copy the keystore; only `apksigner` opens the
explicitly supplied key. It inherits no Java injection or build environment
variables. The trusted system Java must be available on `/usr/bin:/bin`.

Return only the signed APK, its hash and the public certificate fingerprint to the
build/release account. Close the signing session. Review the final release file
list separately; do not upload an entire development `dist` directory. AAB/store
upload signing and Linux/Windows package signing are not implemented by this
helper. iPhone signing is a separate Apple provisioning process, not this Android
key or script; no iPhone runtime behavior changes here.

To include the signed APK in a public Linux or Windows archive, set
`OPHIO_PHONE_APK` to that file and explicitly set `OPHIO_ANDROID_BUILD_TOOLS` to
an absolute SDK build-tools directory, such as `/trusted/android/build-tools/37.0.0`.
Packaging copies the APK into private temporary storage and uses that directory's
`apksigner` and `zipalign` to verify its cryptographic signature, v3 support,
every parsed v2/v3 signer against the source pin, and 16 KiB page alignment.
The archive receives exactly those verified bytes, even if the original APK is
replaced during or after verification. Missing tools, invalid signatures and
mismatched certificates stop packaging; no test signing or tool-location fallback
is attempted. The legacy path-returning `phone_app` helper is personal-only;
public archives must use `phone_app_bytes`. Personal packaging still performs its
existing certificate check without requiring Android SDK tools.

## Which certificate a build trusts

The companion's trust is a source constant, not a runtime setting.
`crates/companion/src/phone/mod.rs` names two literal pins: `PUBLIC_CERT_SHA256`,
the public release certificate above, and `PERSONAL_CERT_SHA256`, the
development certificate of earlier personal builds. `RELEASE_CERT_SHA256`, the
one the phone installer checks, is the public pin in every build unless the
build is made with `OPHIO_PERSONAL_SIGNING=1`, when it is the personal pin
instead. A build trusts exactly one of them, never both. `crates/companion/build.rs`
reads both signing settings, refuses any value other than `0` or `1`, rejects
public mode combined with personal trust, and rebuilds when either changes.
A compile-time assertion also rejects compiler flags that select personal trust
without `OPHIO_PERSONAL_SIGNING=1`, including flags passed directly to `cargo rustc`.

`scripts/release_common.py` reads both literal declarations and checks that
`RELEASE_CERT_SHA256` chooses between them with Rust’s built-in `::core::cfg!`,
not a local macro with the same name. The declarations must not have attributes
that could disable or replace them. The parser ignores
declarations shown inside comments and strings and refuses missing, ambiguous,
nonliteral or equal pins. It then applies the same setting, so packaging checks
an embedded phone app against the pin that build trusts. A personal build that
forgets the setting stops with a message naming it, before the companion is
built, rather than quietly trusting the wrong key. `OPHIO_PUBLIC_RELEASE=1`
refuses `OPHIO_PERSONAL_SIGNING=1`, and requires the public pin to equal
`public-signing.json`.

Linux and Windows archive assembly also checks the companion executable itself,
including when the standalone assembler is used. Before writing to `dist`, it
requires the selected certificate's ASCII fingerprint in the executable and
refuses the other fingerprint. A stale or mixed build must be rebuilt with the
same `OPHIO_PERSONAL_SIGNING` setting used for packaging. The checked bytes are
also the bytes placed in the archive. This catches mismatched build settings;
it is not a signature or a proof of how the executable was built.

First-time beta users have no earlier installation, so the fresh key signs both
v2 and v3 without a lineage, and the single-certificate check is enough. Phones
running a personal build keep the development key and keep updating from
personal builds; moving them to the public key needs the transition below and
the owner's approval, and must not be done by reinstalling or erasing app data.
Agents do not create the protected secret and an approval button in Ophio does
not unlock this signing process. The owner unlocks it directly in the secured
signing session.

## Existing-install rotation remains unfinished

Android's minimum supported version is currently API 29. The existing personal
certificate cannot simply be replaced: that would break upgrades of existing
installs. Plan an owner-approved Android proof-of-rotation lineage from the old
key to the fresh protected key, with backup of the lineage as well as the new
keystore. Any temporary access to the old key for that ceremony is owner-only;
preparatory tests never access it. Review the lineage's capabilities deliberately,
including whether the old signer retains permissions, shared identity or rollback
capabilities. Do not silently authorize rollback to old signed builds.

For a v3 transition across the currently supported phones, explicitly use
`--rotation-min-sdk-version 28`. The tools' default is API 33 with v3.1; that would
continue using the old signer on Android 10–12. The SDK requires the oldest signer
for v1/v2 when a lineage is supplied. Consequently, a compatible APK can contain
the old certificate in its v2 block and the new one in its v3 block even when the
minimum app API is 29. Retaining v2 therefore keeps an owner-controlled old-key
signing dependency for that artifact format; it does not retire the old key.
A deliberate v3-only distribution would need to change the current verifier's
v2 requirement, rather than silently omitting it.

**The companion is not ready for either transition.**
Each build trusts one certificate, `RELEASE_CERT_SHA256`.
`crates/companion/src/phone/apk.rs` and the packaging mirror
`scripts/release_common.py` require v2 and reject any parsed signer other than
that single certificate. Simply changing the pin still rejects an old-v2/new-v3
APK. Before live rotation, implement and review the intended trust transition
without weakening rejection of unrelated certificates. Cover the download,
embedded phone APK and companion update paths, migration ordering and rollback
behavior. Only then perform owner-approved upgrade tests on representative
supported Android versions, preserving app data and checking signing history.
No device installation or uninstall/reinstall is part of this preparation.

The new helper is intentionally **fresh-key only** and refuses rotated/mixed-signer
APKs. It does not create a lineage or rotate an existing installation. Do not use
it for the personal upgrade artifact. The future rotation path must be separately
reviewed and must preserve this public/personal separation.

## Tests and limits

```sh
python3 -m unittest scripts.tests.test_sign_android scripts.tests.test_public_release_verification -v
OPHIO_SIGNING_TEST_SDK=/path/to/sdk \
  python3 -m unittest scripts.tests.test_sign_android scripts.tests.test_public_release_verification -v
```

The second command opts into real SDK 37 signing tests. It generates synthetic
one-day keys under the test process's temporary directory and deletes the entire
fixture afterward. It never locates or reads a real signing key. Tests exercise
fresh-key signing, wrong hash/certificate, already-signed input, output collision,
unattended refusal and an actual synthetic v3 lineage. They verify the rotated
fixture with the Android tools at APIs 29, 32 and 33 and demonstrate that the
current companion packaging check and fresh-key helper reject the mixed signers.
Public archive tests also reject tampered signed payloads despite unchanged
certificate bytes, refuse unavailable tools, and verify that both host archives
keep the checked bytes when the source APK is replaced. Source-pin tests cover
commented and string-contained fake declarations and ambiguous or malformed pins.
These tool checks are **not** proof of an on-device update, protected accounts,
backup recovery, actual key rotation or release readiness.
