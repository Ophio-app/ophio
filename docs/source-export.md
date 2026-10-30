# Preparing a local public-source candidate

This prepares a **local directory for review**, not a release or permission to
publish. It does not create a Git repository, copy Git history, add a remote,
contact GitHub, sign anything, or change repository settings.

Use Python 3.11 or newer and Git. Commit the intended source changes first;
unstaged changes, staged changes and untracked files are not exported. Run from
the source checkout:

```sh
revision="$(git rev-parse --verify HEAD)"
scratch="$(mktemp -d)"
python3 scripts/export_source.py \
  --revision "$revision" \
  --destination "$scratch/source" \
  --receipt "$scratch/source-receipt.json"
```

The revision must be a full commit object ID, not a branch, tag or abbreviation.
The destination must be new or empty and outside every existing Git repository.
Its parent must already exist. The receipt must be a new file outside the export.
Use trusted local parent directories that no other process is modifying. Symlinks
in either output path are refused; the helper does not replace existing files.

## What is checked

The helper creates one archive from the pinned commit and runs the existing
source audit against it before writing the source directory. It refuses findings
such as private-history paths, private-term matches, home-directory references,
key-file names and private-key headers. It also refuses symlinks, hard links,
submodules, unsafe paths and case-insensitive path collisions. Exported bytes
must match the commit's Git blob IDs; archive attributes that omit or rewrite
source files are rejected. The standalone audit performs the same completeness
and byte-integrity check before reporting a pass. Git replacement objects are
ignored. Home-directory exemptions are limited by the trusted audit tool to the
synthetic users `sam` and `you`; the candidate's source policy cannot add users.

All files under `.github/workflows` are **always omitted**, including the
directory itself. There is no option to include workflows. Other source files
are not silently removed or redacted to make a failing audit pass. For a
redacted report of the full committed source, including workflows, run:

```sh
python3 scripts/source_candidate.py --revision "$revision" --require-private-terms
```

The **official export refuses a missing or empty Git-only private-term list**;
there is no fallback that can silently omit that check. Trusted review uses
`--require-private-terms` for the standalone audit too. The list stays outside
the committed source, export and receipt; only a boolean records that it ran.
A fixed fingerprint also catches a retired private name as a case-insensitive
substring in file bytes or paths without placing that name in public code.
Contributors without the private list can run the standalone audit without the
flag and run all synthetic tests, but cannot make an official export. A local
pass without the list does not certify the official privacy check.

## What to retain and review

The private JSON receipt identifies the commit, archive hash and hashes of the
tools used. It lists every exported file with its byte count, normalized mode,
SHA-256 hash and Git blob ID, every directory, and every omitted workflow. The
inventory hash is SHA-256 over the `directories` and `files` fields serialized
with the helper's `json_bytes` function. The source root is private to its owner;
child directories use mode `0755`, and files use `0644` or `0755` according to the
committed executable bit.

Keep the receipt outside the public source. It is local provenance, not a file
to import into a future public repository. Review exactly the directory that the
receipt describes; editing it afterward invalidates that evidence. Re-export
from a new reviewed commit rather than patching a staged candidate.

**Passing pattern checks does not prove that no secrets or private information
remain.** Review source content, binary assets, dependency and license notices,
and public documentation separately. This helper does not build or test the app,
certify platform support, verify signing, or satisfy release qualification.

Before any separately authorized public import, follow the
[public beta CI precautions](operations/public-beta-ci.md), including disabling
GitHub Actions and keeping private runners disconnected. Do not change the
existing repository's visibility or push its history as a shortcut.

## Tests

```sh
python3 -m unittest scripts.tests.test_release_checks.PublicSourceReview \
  scripts.tests.test_source_export scripts.tests.test_source_audit
```

The tests use synthetic local Git repositories and do not need credentials,
network access, a build toolchain, or signing keys.
