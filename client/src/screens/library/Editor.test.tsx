// SPDX-License-Identifier: Apache-2.0
import { cleanup, render } from "@testing-library/react";
import { EditorView } from "@codemirror/view";
import { afterEach, expect, it } from "vitest";
import Editor from "./Editor";
import { forSave } from "./model";

afterEach(cleanup);

// The companion reads and saves a note's bytes as they are, mixed line
// endings and lone carriage returns included. The editor itself splits on
// every break and hands back "\n"; what is saved from it changes only what
// was edited.
it("saves an edit with the note's own line endings", () => {
  const text = "# Plan\r\nfirst\nsecond\r\nprogress 50%\rprogress 100%\r\n";
  let changed = "";
  const { container } = render(
    <Editor initialText={text} onChange={(t) => (changed = t)} onComposing={() => {}} />,
  );
  const view = EditorView.findFromDOM(container.querySelector<HTMLElement>(".cm-editor")!)!;
  view.dispatch({ changes: { from: view.state.doc.length, insert: "done" } });
  expect(changed).not.toBe(`${text}done`);
  expect(forSave(changed, "mixed", text)).toBe(`${text}done`);
});
