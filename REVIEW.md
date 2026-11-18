<!-- SPDX-License-Identifier: Apache-2.0 -->
# Changes that need particular care

These checks make changes visible; they do not approve themselves. A passing
check does not replace reviewing why a new permission or exception is needed.

## Protected paths

- `scripts/policies/crate-dependencies.toml` defines which workspace crates may
  depend on each other, including tests, optional features and every platform.
  Review additions as architecture changes. The four existing groups of
  questionable dependencies are recorded there for separate decisions.
- `scripts/policies/lint-suppressions.json` records the accepted maximum number
  of each lint suppression. Do not raise a number merely to make a check pass.
  Explain why the extra suppression is necessary, and prefer fixing the cause.
- `client/src/host/requestPolicy.ts` records feature requirements and reasoned
  compatibility exceptions for phone requests. New functionality should name
  the feature the computer advertises; exceptions need a real compatibility
  reason, not an empty explanation.

Changes to these files and the scripts that enforce them need review together.

## Fast local checks

Run `bash scripts/ci.sh quick` before a longer build. It checks the phone's
locked Rust dependencies, workspace dependency permissions, lint suppression
counts and phone request coverage. It does not build or install the app.

A missing cached phone dependency is not a passing lock check. Run
`cargo fetch --locked --manifest-path client/src-tauri/Cargo.toml` with the
network available, then retry. If the lock itself is stale, regenerate it and
commit `client/src-tauri/Cargo.lock` along with the manifest change.
