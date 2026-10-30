// SPDX-License-Identifier: Apache-2.0
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import type { SpeechEvent } from "../../host/link";
import type { HostState } from "../../state/projection";
import { emptyHostState } from "../../state/projection";
import { AnnotationStore, markLabel, nextMarkIndex } from "../../state/annotations";
import type { SpeechSegment } from "../../protocol/types";
import { Speech } from "./Speech";

let listener: ((event: SpeechEvent) => void) | undefined;
const platform = {
  speech: () => ({
    send: () => true,
    close() {},
    listen: (l: (event: SpeechEvent) => void) => {
      listener = l;
      return () => {
        listener = undefined;
      };
    },
  }),
};
vi.mock("../../app/context", () => ({
  useApp: () => ({ platform, profile: { profileId: "phone", name: "studio" }, ui: { push() {} } }),
  useHost: <T,>(select: (s: HostState) => T) =>
    select({
      ...emptyHostState,
      capabilities: { speech: { state: "available" } } as HostState["capabilities"],
      readiness: {
        checks: [{ area: "speech", label: "Speech", status: "passed", detail: null, remedy: null }],
      } as HostState["readiness"],
    }),
  useLink: () => ({ state: "online" }),
}));
vi.mock("../../shell/Screen", () => ({ useRoute: () => ({ active: true }) }));

beforeEach(() => {
  localStorage.clear();
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
});
afterEach(cleanup);

const said = (segment_id: string, text: string, start_ms: number): SpeechSegment => ({
  segment_id, text, start_ms, end_ms: start_ms + 1500, revision: 1, is_final: true,
});

it("undoing a mark keeps the words spoken after it", () => {
  const store = new AnnotationStore("speech-undo");
  const draft = store.create("project", null);
  render(<Speech draft={draft} store={store} recordingTarget={null} onRecording={() => {}} />);
  fireEvent.click(screen.getByRole("button", { name: "Speak" }));
  fireEvent.click(screen.getByRole("button", { name: "Start recording" }));
  act(() => listener?.({ type: "state", state: { state: "online", since: 0 } }));

  act(() => listener?.({ type: "text", frame: { type: "segment", segment: said("1", "The heading is too small", 0) } }));
  // While talking, the person draws a mark (Annotate's add), then a second phrase arrives.
  store.update(draft.id, (d) => ({
    ...d,
    marks: [...d.marks, {
      mark_id: "mark-a", label: markLabel(nextMarkIndex(d)), color: 0, source_key: "S1",
      shape: { kind: "pin", at: { x: 0.5, y: 0.5, t: null } }, created_at: Date.now(), deleted: false,
    }],
  }));
  act(() => listener?.({ type: "text", frame: { type: "segment", segment: said("2", "and it should be blue", 2000) } }));

  // The mark was misplaced: Undo removes it…
  store.undo(draft.id);
  expect(store.get()[draft.id]?.marks).toEqual([]);
  // …but must not throw away what the person said after drawing it.
  expect(store.get()[draft.id]?.comments.map((c) => c.text)).toEqual([
    "The heading is too small",
    "and it should be blue",
  ]);
  // Redo brings the mark back with both spoken comments.
  store.redo(draft.id);
  expect(store.get()[draft.id]?.marks.map((m) => m.mark_id)).toEqual(["mark-a"]);
  expect(store.get()[draft.id]?.comments).toHaveLength(2);
});
