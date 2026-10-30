# Fuzz targets

Coverage-guided fuzzing for parsers that read input from outside the owner's
control. The crate is its own Cargo workspace, so the product's lockfile and
builds never include it. It needs a nightly toolchain and `cargo-fuzz`.

```sh
cargo +nightly fuzz run frames -- -max_total_time=600
```

| Target | Reads | Checks |
| --- | --- | --- |
| `frames` | JSON frames on every connection, both directions | integer bound first; a frame that reads writes back to the same value within the bound |
| `binary_frames` | TDV1 tile frames and terminal output frames | a frame that parses rebuilds into one that parses the same |
| `library_path` | Library paths a phone sends | an accepted path is already normal and stays inside its root |
| `tool_approval` | an agent's permission request | a bounded preview, and a raw request that is JSON reading back to itself |

Parsers private to a crate, such as the Claude Code and Codex readers, are
covered by generated-input tests in that crate instead (see
`test_support::json::Variants`).

Corpora and crash artifacts stay out of the repository. When a target
finds a defect, fix it with a regression test that uses the minimised input.
