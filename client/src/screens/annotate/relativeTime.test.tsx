// SPDX-License-Identifier: Apache-2.0
//
// Stroke and speech times are milliseconds since the draft began, carried
// as u32 by the computer. The phone's clock can be set back after a draft is
// made, and a draft can outlive that range; a value outside it makes the
// whole message unreadable to the computer.
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { SpeechEvent } from "../../host/link";
import type { MarkShape } from "../../protocol/types";
import { AnnotationStore, type SourceDraft } from "../../state/annotations";
import { emptyHostState, type HostState } from "../../state/projection";
import { Canvas } from "./Canvas";
import { FIT } from "./canvas";
import { Speech } from "./Speech";

let listener: ((event: SpeechEvent) => void) | undefined;
const platform = {
  speech: () => ({
    send: () => true,
    close() {},
    listen: (l: (event: SpeechEvent) => void) => {
      listener = l;
      return () => { listener = undefined; };
    },
  }),
};
vi.mock("../../app/context", () => ({
  useApp: () => ({ platform, profile: { profileId: "phone", name: "studio" }, ui: { push() {} } }),
  useHost: <T,>(select: (s: HostState) => T) =>
    select({
      ...emptyHostState,
      capabilities: { speech: { state: "available" } } as HostState["capabilities"],
      readiness: { checks: [{ area: "speech", label: "Speech", status: "passed", detail: null, remedy: null }] } as unknown as HostState["readiness"],
    }),
  useLink: () => ({ state: "online" }),
}));
vi.mock("../../shell/Screen", () => ({ useRoute: () => ({ active: true }) }));
vi.mock("./useSourceUrl", () => ({ useSourceUrl: () => ({ url: "blob:picture", error: null }) }));

const U32_MAX = 2 ** 32 - 1;
const inRange = (n: number | null | undefined) => typeof n === "number" && Number.isInteger(n) && n >= 0 && n <= U32_MAX;
const source: SourceDraft = { key: "S1", blobKey: "blob", artifactId: null, sha256: null, width: 320, height: 220,
  mediaType: "image/png", durationMs: null, origin: { kind: "phone_import", original_name: null }, capturedAt: 0, derivedFrom: null, derivation: null };

beforeEach(() => {
  localStorage.clear();
  vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
  vi.stubGlobal("PointerEvent", class extends MouseEvent {
    pointerId: number; isPrimary: boolean;
    constructor(type: string, init: PointerEventInit) { super(type, init); this.pointerId = init.pointerId ?? 1; this.isPrimary = init.isPrimary ?? true; }
  });
  SVGElement.prototype.setPointerCapture = vi.fn();
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
});
afterEach(() => { cleanup(); vi.unstubAllGlobals(); });

it.each([
  ["the clock was set back after the draft was made", 60_000],
  ["the draft is older than fifty days", -51 * 24 * 3600 * 1000],
])("a stroke drawn when %s keeps times the computer can read", (_, fromNow) => {
  const onAdd = vi.fn();
  render(<Canvas source={source} marks={[]} tool="stroke" selected={[]} viewport={FIT} clock={Date.now() + fromNow}
    onAdd={onAdd} onMove={() => {}} onSelect={() => {}} onViewport={() => {}} />);
  const canvas = screen.getByRole("img");
  fireEvent.pointerDown(canvas, { pointerId: 1, isPrimary: true, clientX: 50, clientY: 50 });
  fireEvent.pointerMove(canvas, { pointerId: 1, isPrimary: true, clientX: 80, clientY: 60 });
  fireEvent.pointerUp(canvas, { pointerId: 1, isPrimary: true, clientX: 90, clientY: 70 });
  const shape = onAdd.mock.calls[0]?.[0] as MarkShape;
  expect(shape.kind).toBe("stroke");
  if (shape.kind === "stroke") for (const p of shape.points) expect(inRange(p.t)).toBe(true);
});

it("words spoken after the clock was set back keep times the computer can read", () => {
  const store = new AnnotationStore("relative-time");
  const draft = { ...store.create("project", null), createdAt: Date.now() + 60_000 };
  store.record(draft.id, () => draft);
  render(<Speech draft={draft} store={store} recordingTarget={null} onRecording={() => {}} />);
  fireEvent.click(screen.getByRole("button", { name: "Speak" }));
  fireEvent.click(screen.getByRole("button", { name: "Start recording" }));
  act(() => listener?.({ type: "state", state: { state: "online", since: 0 } }));
  act(() => listener?.({ type: "text", frame: { type: "segment", segment: { segment_id: "1", text: "The heading is too small", start_ms: 0, end_ms: 1500, revision: 1, is_final: true } } }));
  const segments = store.get()[draft.id]?.speech?.segments ?? [];
  expect(segments).toHaveLength(1);
  for (const s of segments) expect(inRange(s.start_ms) && inRange(s.end_ms)).toBe(true);
});
