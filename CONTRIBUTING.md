# Contributing

Everyone taking part follows the [Code of Conduct](CODE_OF_CONDUCT.md).
Report security problems as [SECURITY.md](SECURITY.md) describes, never in a
public issue.

## How changes reach this repository

Øphio is developed in a private copy, and this repository receives one commit
for each release. So a pull request here is not merged directly:

1. A maintainer reviews it here, as usual.
2. If it is accepted, it is applied to the development copy with you kept as
   its author, or as a co-author when it had to be adapted.
3. It reaches this repository with the next release, which names it in
   [CHANGELOG.md](CHANGELOG.md), and the pull request is closed with a link to
   that release.

Issues work the same way: a fix appears in a later release, and the issue is
closed when it does.

## Licence and code

Contributions intentionally submitted for inclusion are provided under
Apache License 2.0, section 5, unless explicitly stated otherwise. No DCO sign-off
or contributor licence agreement is required. Preserve third-party attribution
and identify material you did not author.

Use Rust edition 2024 and the documented minimum Rust version. Keep transport
code behind the application boundary and preserve grants, lease fencing,
revision checks and honest unknown outcomes. Add a visible Apache-2.0 SPDX header
to owned source files. Avoid secrets, personal paths, account data and screenshots
in source, tests and diagnostic output.

Use `cargo fmt --all`, keep Clippy warning-free, and run the affected tests plus
dependent integration tests. New behavior and bug fixes need deterministic tests
with fakes, temporary files and loopback. Tests must not reach a person's real
desktop, account, paid model or external chat. Follow [BUILDING.md](BUILDING.md)
for exact commands, packaging checks and isolated native test boundaries.

`scripts/pre-push` runs the quick checks a push needs, in seconds: the source
scans always, then `cargo fmt --check`, the TypeScript type check or the script
tests when the pushed commits change Rust, the client or `scripts/`. To have
git run it before every push, from any worktree, add this as
`.git/hooks/pre-push` and make it executable:

```sh
#!/bin/sh
top=$(git rev-parse --show-toplevel) && [ -x "$top/scripts/pre-push" ] && exec "$top/scripts/pre-push" "$@"
exit 0
```

Skip it for one push with `git push --no-verify` or `OPHIO_SKIP_PRE_PUSH=1`.
CI still runs every stage the change needs.

Before proposing a dependency, check its licence and maintenance status. Declare
an explicit version in the owning crate, update its lockfile, and rerun the
licence inventory. Do not treat a package's top-level licence as evidence for
every bundled font, model, native library or copied file.

Describe the concrete behavior changed, relevant constraints, tests actually
run, and any platform checks still unrun. Keep changes focused and preserve
public documentation without internal project material.
