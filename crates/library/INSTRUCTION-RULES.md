# Instruction discovery rules

Verified against official documentation on **2026-09-27**. This inspector reports
candidate files and observed revisions, not an execution trace or proof of reload.
The host supplies effective home paths, fallback names and byte limits through
`InstructionConfig`; the adapter does not read native settings or credentials.

## Claude Code

User instructions precede the ancestor chain, which runs from the filesystem root
to the working directory. Each directory contributes `CLAUDE.md`,
`.claude/CLAUDE.md` and `CLAUDE.local.md` candidates, with local notes last.
The inspector expands `@` imports relative to their containing file, including
absolute and home-relative references, suppresses cycles, and stops after four
hops. Code spans and fenced code are skipped. Resource definitions follow the
instruction chain: user skills/agents, then repository-to-working-directory
skills/agents. These are available definitions, not automatically activated work.

Sources: [memory and imports](https://code.claude.com/docs/en/memory),
[skill discovery](https://code.claude.com/docs/en/skills),
[agent discovery](https://code.claude.com/docs/en/sub-agents).

The native tool may require approval for external imports. It also supports
conditional rules, managed policy, additional directories, plugins, worktree
fallbacks and configurable AGENTS.md discovery. This adapter does not evaluate
those settings or predict their runtime effects. Its import tokenizer is
conservative and is not the native Markdown parser. Harness-reported paths absent
from the static candidates are still included as `Loaded`.

## Codex

The user file is the first nonempty `AGENTS.override.md` or `AGENTS.md` in the
configured Codex home. Project lookup starts at the nearest ancestor containing
`.git`, and proceeds to the working directory; without that marker, only the
working directory participates. Each directory selects one nonempty file:
`AGENTS.override.md`, `AGENTS.md`, then configured fallback names. The combined
budget defaults to 32 KiB. Partial candidates show the budgeted byte count in
their label; omitted candidates are `Unknown`, unless the harness reports them
loaded. The full disk revision is always distinct from that budget estimate.

Sources: [instruction discovery and limits](https://learn.chatgpt.com/docs/agent-configuration/agents-md),
[local skill directories](https://learn.chatgpt.com/docs/build-skills).

Suggested skill roots include `.agents/skills` in the project and home.
An existing configured Codex-home `skills` directory is also offered for legacy
installations; its presence is not proof that the current native tool loads it.
No Codex agent-definition directory is invented.

## Authority and observations

`Discovered` means readable now, `Missing` means absent, and `Unknown` covers
inaccessible, oversized, unsafe or ungranted candidates. `Loaded` requires the
caller's native report. `revision_at_start` remains absent: a path report alone
cannot establish which historical bytes were loaded. The inspector never infers
`Changed` without that evidence.

Project contents and imported resources require registered roots. Native home
inspection is restricted to Markdown instruction/resource paths under the
explicitly supplied homes. Other imports remain visible as unknown without
reading their contents. Symlinks are deliberately not followed. Enumeration is
bounded; saves and candidate discovery do not register roots, execute resources,
restart harnesses or promise activation. Tests inject fixture homes without
changing the process environment or opening the real home directory.
