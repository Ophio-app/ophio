# Annotation and connections integration

A draft starts on the phone: `annotationStore(profileId).create(projectId,
taskId)` persists it, and the caller pushes the `annotate` route with its ID
(Chat's composer and a receipt's correction do this). Sources are added inside
the screen through `importSource` and `captureSource` from `sources.ts`; capture
is explicit and never sends input or a task. The caller owns any desktop input
pause. Capture failure rejects with a `SourceError`; the draft remains available
for recovery.

`AnnotationSummaryRow` is available for draft lists. Optional draft fields let
old drafts migrate on load. Undo keeps mark IDs and monotonically increasing labels. Draft metadata and
image bytes use local storage and IndexedDB respectively; persistence failures
are visible. Clip position, trim fields, typed comments and target selections
survive reopening.

Review remains the only agent-send path. The composer saves `annotationId` into
the matching Chat draft and opens Review. `toBundle` snapshots the content before
uploads, rejects unfinished comments/links/steps, excludes video and excluded
original images, and includes edited speech wording without audio. After the
accepted send, call `markSent` with the same store and ID. Expected/Actual and mark
descriptions are serialized into `general_note`. Serialize Review sends for each
draft: the prepared bundle cache keeps its most recent pending revision.

Connections uses existing commands and readiness UI. Access changes wait for
receipts; an interrupted receipt remains unknown. Export uses an explicit
allowlist of annotation metadata, Chat draft text/attachments and voice settings.
It never enumerates storage or exports pairing credentials. The app saves the
export through the platform's `saveToPhone` (Downloads on Android) and shows
where it went; a plain browser falls back to `screens/settings/helpers.ts:saveBlob`.

## Limits

- Inbox has no metadata-store contract, so export excludes Inbox records and
  says exactly what it includes.
- No app update mechanism is configured, and the settings screen does not
  claim one.
- Expected/Actual travel in `general_note`; the wire types have no fields of
  their own for them.
- Frame nudges use a disclosed 1/30-second estimate because browser video
  metadata does not expose source frame rate. Extraction uses the decoded frame.
- Preview host classification is a warning hint, not a DNS or network security
  boundary; the computer must enforce its URL policy.
- Capture rejects a target geometry change observed during the request. Exact
  captured geometry provenance would require metadata from the computer.
- Android microphone permissions, native pickers/downloads, true keyboard
  insets and touch/pinch need device validation. Browser tests use headless
  Chromium, a reduced viewport for keyboard space, and simulated speech.

## Verification

Run these from `client/`:

```sh
npm run typecheck && npm test
npx vite build
npx vite --config src/screens/annotate/vite-check.config.mjs --mode sim --host 127.0.0.1 --port 5804 --strictPort
node src/screens/annotate/ui-check.mjs http://127.0.0.1:5804
node scripts/shots.mjs http://127.0.0.1:5804 /tmp/annotation-shots/verified scripts/plans/annotation-and-connections.json
```

The local Vite config also allows font assets when `node_modules` is a symlink. The browser assertion script checks actual pointer drawing,
unchanged normalized coordinates and source identity after rotation/pan/zoom,
zero input or microphone calls on the typed path, keyboard focus while choosing
targets, linked deletion/undo, reload durability, decoded redaction pixels and
excluded originals, chat-link rejection/acceptance and revocation.

Formatting and linting use already installed tools.
`$PRETTIER` is an installed Prettier's `bin/prettier.cjs`; `$LINT_MODULES` is a
`node_modules` directory that contains ESLint:

```sh
node "$PRETTIER" --check src/screens/annotate src/screens/settings src/state/annotations.ts src/dev/sim/annotate.ts src/dev/sim/settings.ts scripts/plans/annotation-and-connections.json
ANNOTATE_LINT_MODULES="$LINT_MODULES" node "$LINT_MODULES/eslint/bin/eslint.js" --max-warnings 0 --config src/screens/annotate/lint.config.mjs src/screens/annotate/*.ts src/screens/annotate/*.tsx src/screens/settings/*.ts src/screens/settings/*.tsx src/state/annotations.ts src/dev/sim/annotate.ts src/dev/sim/settings.ts
git diff --check
```

The optional lint config finds its modules through `ANNOTATE_LINT_MODULES`.
