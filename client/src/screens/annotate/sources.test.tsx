// SPDX-License-Identifier: Apache-2.0
// Images from the phone: several chosen at once are added in order, while
// the message has room for them.
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import type { Platform } from "../../host/link";
import { HostSession } from "../../host/session";
import { annotationStore, blobs, MAX_SOURCES, outgoingSources, useAnnotation, type AnnotationStore, type SourceDraft } from "../../state/annotations";
import { emptyHostState } from "../../state/projection";
import { Sources } from "./Sources";

vi.mock("./useSourceUrl", () => ({ useSourceUrl: () => ({ url: null, error: null }) }));

let app: App;
let store: AnnotationStore;
let id: string;
beforeEach(() => {
  localStorage.clear();
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  let stored = 0;
  vi.spyOn(blobs, "put").mockImplementation(async () => `blob-${++stored}`);
  // A file named "broken" can't be decoded.
  vi.stubGlobal("createImageBitmap", async (blob: Blob) => {
    if (blob instanceof File && blob.name.startsWith("broken")) throw new Error("undecodable");
    return { width: 4, height: 3, close() {} };
  });
  const profileId = `sources-${Math.random()}`;
  const session = new HostSession(profileId, { connect() {}, disconnect() {}, send: () => false, listen: () => () => {} });
  session.state.set({ ...emptyHostState, readiness: { checked_at: 0, checks: [] } });
  app = {
    session, ui: new UiStore(profileId), profiles: [], switchTo() {}, pairAnother() {}, async rename() {}, forget: async () => {},
    profile: { profileId, hostId: "host", deviceId: "phone", name: "Computer", addresses: [], certSha256: "", pairedAt: 0 },
    platform: { kind: "simulated", openExternal: vi.fn() } as unknown as Platform,
  };
  store = annotationStore(profileId);
  id = store.create("project", null).id;
});
afterEach(() => { cleanup(); vi.unstubAllGlobals(); vi.restoreAllMocks(); });

function Harness() {
  const d = useAnnotation(store, id);
  return d && <Sources draft={d} store={store} />;
}
const draft = () => store.get()[id]!;
const image = (name: string) => new File([name], name, { type: "image/png" });
const existing = (n: number): SourceDraft[] => Array.from({ length: n }, (_, i) => ({
  key: `S${i + 1}`, blobKey: `old-${i}`, artifactId: null, sha256: "abc", byteSize: 1, mediaType: "image/png", width: 4, height: 3,
  durationMs: null, origin: { kind: "phone_import", original_name: null }, capturedAt: 1, derivedFrom: null, derivation: null, included: true,
}));
async function choose(files: File[]) {
  render(<AppProvider app={app}><Harness /></AppProvider>);
  fireEvent.click(screen.getByRole("button", { name: "Add source" }));
  const input = screen.getByLabelText("Image files from this device") as HTMLInputElement;
  expect(input.multiple).toBe(true);
  Object.defineProperty(input, "files", { configurable: true, value: files });
  await act(async () => { fireEvent.change(input); });
  // Each is hashed and saved in turn.
  for (let i = 0; i < 20; i++) await act(() => new Promise((done) => setTimeout(done, 0)));
}

it("adds every image chosen at once, in order, and starts on the first", async () => {
  await choose([image("one.png"), image("two.png"), image("three.png")]);
  expect(draft().sources.map((s) => [s.key, s.origin])).toEqual([
    ["S1", { kind: "phone_import", original_name: "one.png" }],
    ["S2", { kind: "phone_import", original_name: "two.png" }],
    ["S3", { kind: "phone_import", original_name: "three.png" }],
  ]);
  expect(outgoingSources(draft())).toHaveLength(3);
  expect(draft().selectedSource).toBe("S1");
  // The sheet closes and says nothing more.
  expect(screen.queryByRole("button", { name: "Images from this device" })).toBeNull();
  expect(document.querySelector(".notice")).toBeNull();
});

it("adds only as many as the message has room for, and says how many were left", async () => {
  store.record(id, (d) => ({ ...d, sources: existing(MAX_SOURCES - 2), nextSource: MAX_SOURCES - 1 }));
  await choose([image("a.png"), image("b.png"), image("c.png"), image("d.png")]);
  expect(outgoingSources(draft())).toHaveLength(MAX_SOURCES);
  expect(draft().sources.slice(-2).map((s) => s.origin)).toEqual([
    { kind: "phone_import", original_name: "a.png" },
    { kind: "phone_import", original_name: "b.png" },
  ]);
  expect(screen.getByText(`2 images weren’t added: a message holds ${MAX_SOURCES} images. Leave some out under Image options to add others.`)).toBeTruthy();
});

it("keeps the images that could be added when one can't", async () => {
  await choose([image("good.png"), image("broken.png"), image("fine.png")]);
  expect(draft().sources.map((s) => s.origin)).toEqual([
    { kind: "phone_import", original_name: "good.png" },
    { kind: "phone_import", original_name: "fine.png" },
  ]);
  expect(screen.getByText(/^1 file couldn’t be added\. This image couldn’t be decoded/)).toBeTruthy();
});

it("keeps the sheet open with the reason when none could be added", async () => {
  await choose([image("broken.png")]);
  expect(draft().sources).toEqual([]);
  expect(screen.getByRole("alert").textContent).toMatch(/This image couldn’t be decoded/);
  expect(screen.getByRole("button", { name: "Images from this device" })).toBeTruthy();
});

it("counts a still placed while the images are being added", async () => {
  store.record(id, (d) => ({ ...d, sources: existing(MAX_SOURCES - 2), nextSource: MAX_SOURCES - 1 }));
  const removed = vi.spyOn(blobs, "remove").mockResolvedValue();
  // A video comment's still lands while the second image is being saved.
  vi.spyOn(blobs, "put").mockImplementation(async (blob) => {
    const name = (blob as File).name;
    if (name === "b.png")
      store.record(id, (d) => ({
        ...d,
        sources: [...d.sources, { ...existing(1)[0]!, key: "still", blobKey: "still", origin: { kind: "shown", original_name: null } as never }],
      }));
    return `blob-${name}`;
  });
  await choose([image("a.png"), image("b.png")]);
  expect(outgoingSources(draft())).toHaveLength(MAX_SOURCES);
  expect(draft().sources.slice(-2).map((s) => s.key)).toEqual([`S${MAX_SOURCES - 1}`, "still"]);
  expect(removed).toHaveBeenCalledWith("blob-b.png");
  expect(screen.getByText(`1 image wasn’t added: a message holds ${MAX_SOURCES} images. Leave some out under Image options to add others.`)).toBeTruthy();
});

it("adds the images chosen under Attach as soon as the drawing opens", async () => {
  app.ui.requestSource(id, { kind: "files", files: [image("one.png"), image("two.png")] });
  render(<AppProvider app={app}><Harness /></AppProvider>);
  for (let i = 0; i < 20; i++) await act(() => new Promise((done) => setTimeout(done, 0)));
  expect(draft().sources.map((s) => s.origin)).toEqual([
    { kind: "phone_import", original_name: "one.png" },
    { kind: "phone_import", original_name: "two.png" },
  ]);
  expect(draft().selectedSource).toBe("S1");
  // Nothing more to choose, and it isn't added twice.
  expect(screen.queryByRole("button", { name: "Images from this device" })).toBeNull();
  expect(app.ui.claimSource(id)).toBeNull();
});

it("opens at the capture choices when Screen was chosen under Attach", () => {
  app.ui.requestSource(id, { kind: "screen" });
  render(<AppProvider app={app}><Harness /></AppProvider>);
  expect(screen.getByRole("dialog", { name: "Add a source" })).toBeTruthy();
  expect(screen.getByRole("button", { name: "Capture the computer" }).getAttribute("aria-expanded")).toBe("true");
});
