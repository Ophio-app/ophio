# Library and transfers integration

## Reusable entry points

- `../transfers/TransferRow.tsx`: named `TransferRow` export for local jobs and computer history records.
- `../transfers/DestinationPicker.tsx`: named `DestinationPicker` export, controlled by a `TransferDestination` value.
- `../transfers/manager.ts`: `transferManager(app)`, `startUpload`, `startDownload`, `pause`, `continue`, `cancel`, and a reactive `jobs` store. Jobs continue independently of screen navigation. Artifact uploads expose `artifactId` after confirmation.
- `../transfers/saveToPhone.ts`: saves a verified download through `Platform.saveToPhone`, which on Android writes to Downloads through MediaStore and reports the saved name. The job keeps that as `savedAs` so its row can say where the file went. A platform without a saver reports its absence and keeps the verified bytes in the tray.
- `shared.tsx`: breadcrumbs, compact line differences, connection notices, and screen focus helpers remain local; promote only if useful to other screens.

## Known limits

The current command schema has no folder creation, folder rename, or folder delete command. Those actions are explicitly unavailable. File creation, rename, and delete are implemented; mutations never infer an overwrite.

Confirmed file creation clears that folder’s New file name and opens the returned file path on the originating tab only while the same folder route is still current. A late reply preserves navigation through Safety controls or to another folder. Failed or unconfirmed creation keeps the name in the sheet for recovery. The root and nested-folder flows, original tab, deferred replies after navigation, duplicate names, and lost receipts are covered in `recovery.test.tsx`.

`FileContent` has no explicit too-large flag. The editor enforces a 512 KiB limit, offers downloads for larger files, and keeps unsupported encodings and mixed newline files read only. CRLF/LF and the original UTF-8 BOM are preserved on save. Creating a sibling copy has no BOM option in the protocol; the review discloses that a new copy is UTF-8 without a BOM.

The existing CodeMirror dependency produces a separate 500.68 kB minified / 174.52 kB gzip editor chunk and Vite's size warning. It is fetched only when the editor opens. Physical Android keyboard, native storage and share intake are untested on physical devices.

The shell gives a narrow screen's title its own row and packs the attention entry at every destination; library rows hide their icons and chevrons when narrow. Landscape remains scrollable but the shared navigation and attention bar leave limited reading height.

## Verification

Run from `client/`:

```sh
npm run typecheck
npm test
npx vite build
node src/screens/library/lint.mjs "$ESLINT" "$TS_ESTREE" "$VISITOR_KEYS"
node "$PRETTIER" --check 'src/screens/library/**/*.{ts,tsx,css,mjs,md}' 'src/screens/transfers/**/*.{ts,tsx,css}' src/dev/sim/library.ts src/dev/sim/transfers.ts scripts/plans/library-and-transfers.json
node scripts/shots.mjs http://127.0.0.1:5801 /tmp/library-shots scripts/plans/library-and-transfers.json
git diff --check
```

The lint/format commands take the paths of already installed tools (ESLint, `@typescript-eslint/typescript-estree` and `@typescript-eslint/visitor-keys` entry points, and Prettier's `bin/prettier.cjs`); they install nothing. The local lint runner applies six correctness rules, supplemented by strict TypeScript and the repository's unused-symbol checks.

When `node_modules` is a symlink, Vite rejects the font files it resolves to. This runtime-only server invocation allows the resolved dependency directory without changing configuration:

```sh
node --input-type=module - <<'JS'
import { createServer } from 'vite';
import { realpathSync } from 'node:fs';
const server = await createServer({ mode: 'sim', server: {
  host: '127.0.0.1', port: 5801, strictPort: true,
  fs: { allow: [process.cwd(), realpathSync('node_modules')] }
}});
await server.listen();
server.printUrls();
JS
```

Headless shots cover 45 scenarios, including receipt loss, conflicting edits, merge/copy recovery, reconnect, read errors, file mutation review, 3 MB uploads, numbered collisions, resume, verified downloads, light/dark, 360 px at 200% text, landscape, and wide layouts. Images are intentionally outside the repository.

The simulator exposes `__sim.library.editOnComputer(rootId, path, text)`, `uncertainNextSave()`, `deny(rootId)`, `allow(rootId)`, and `__sim.transfers.chunkDelay(milliseconds)`. These controls are isolated to simulation.

Checking an uncertain save reconciles the latest draft. Confirmation of the earlier text rebases newer edits onto the confirmed revision and leaves the editor open. Changed, missing and oversized replies keep newer text; replies for discarded, moved or superseded drafts leave them alone. Deferred reads and persistence are covered in `save.test.ts` and the current editor in `conflictResolve.test.tsx`.

Conflict merging checks UTF-8, BOM, newline and escaped command sizes before replacing the original draft. A merge that cannot be reopened and saved stays in conflict with copy recovery available. Oversized host conflicts already omit the inline current file through `conflict_fitted`; the client preserves `currentTooLarge` and offers recovery on the computer.
