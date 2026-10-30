# Distribution inputs

The Python scripts read installed package sources and lockfiles offline and add
no runtime dependency. All source paths in generated
inventories are relative or upstream URLs; local registry/cache paths are not
distribution metadata.

`license-evidence.toml` supplements licence texts omitted from crate archives.
Each entry binds an exact package version to a source URL, SHA-256 and reason.
Most URLs use the exact revision in the crate's `.cargo_vcs_info.json`. Selectors
references the standard MPL-2.0 text from its source headers. These entries do
not exempt a dependency from policy or change its licence expression.

`components.toml` records native/embedded components that package metadata alone
does not cover. Speech URLs, revisions, sizes and hashes are read from the source
catalog at generation time. SQLite's version, disclaimer and source hash are
read from the actual bundled amalgamation. Font hashes are matched to the built
web assets. Lucide's complete notice also preserves the MIT Feather attribution.

The native notice baselines were collected from PipeWire's installed COPYING,
LLVM/MinGW's supplied LICENSE.TXT and runtime aggregate, and jpeg-encoder 0.6.1's
embedded IJG transform header. [Whisper's upstream licence](https://github.com/openai/whisper/blob/main/LICENSE)
is included for the external runtime model inventory. Licence texts are upstream
material and retain their original terms; they are not relicensed Apache-2.0.

The MinGW runtime's notice covers far more than a program links, including
Cephes-derived maths whose grant it leaves unresolved and Wine material under the
LGPL. Windows packaging links each executable with a map, and
`scripts/windows_runtime.py` stops packaging if any runtime object is linked that
`windows/runtime-objects.toml` has not reviewed. The reviewed objects are public
domain, ZPL-2.1 (the runtime's overall licence) or David M. Gay's gdtoa under
Lucent's permission notice, and the MinGW component declares exactly those. The
full upstream notice still ships unchanged. Before listing another object, read
its source and the files it includes; never add an entry that merely labels
unknown terms permissive.

MPL/LGPL components are reported for review, even when a licence check otherwise
passes. The MPL-2.0 crates are used unmodified: the notices carry the licence text
and each crate's exact source archive URL, whose checksum the SBOM records. If an
MPL-covered file is ever modified, its modified source must be published too. Any
LGPL component still needs its replacement/relinking obligations reviewed before
distribution. No policy exceptions are currently configured. Native Windows
behavior is unqualified, and physical phone interoperability is only partly
hand-tested; see `SUPPORT.md`.

## Getting Øphio for the computer from the phone app

Before `scripts/package-android.sh`, set `OPHIO_DOWNLOAD_URL` to the owner's
chosen HTTP(S) download page. Optionally set `OPHIO_CHECKSUM_URL` to its
checksum page. These may be exported in `BUILD_ENV_FILE`, next to the SDK/NDK
settings. `scripts/package_android.py` passes them to Vite as
`VITE_OPHIO_DOWNLOAD_URL` and `VITE_OPHIO_CHECKSUM_URL`; changing the
channel requires rebuilding, with no source edit. Do not put credentials in URLs.

For a direct client build or a headless simulator, supply the `VITE_` variables
when invoking npm/Vite. No destination is hard-coded. When unset, Welcome and
pairing help explicitly report that this build has no download link. They always
offer plain-text installation and pairing steps through the platform share sheet
(Android ACTION_SEND, browser Web Share) or the clipboard where sharing is absent.
Choosing the public distribution URL and publishing the archives remain owner decisions.
