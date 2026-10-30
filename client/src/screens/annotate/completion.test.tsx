// SPDX-License-Identifier: Apache-2.0
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
} from "@testing-library/react";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { HostSession } from "../../host/session";
import type { Platform } from "../../host/link";
import { emptyHostState } from "../../state/projection";
import {
  annotationStore,
  AnnotationStore,
  toBundle,
  type SourceDraft,
  type AnnotationDraft,
} from "../../state/annotations";
import { RouteProvider } from "../../shell/Screen";
import type { ArtifactMeta } from "../../protocol/types";
import { captureProvenance } from "./sources";
import {
  applyClipTrim,
  boundedMoment,
  clipRange,
  reproductionProblem,
} from "./trim";
import { ReproductionReceipt, ReproductionReview } from "./ReproductionReview";
import Annotate from "./Annotate";
import { PreviewCard, previewAddress } from "../control/Previews";

vi.mock("./useSourceUrl", () => ({
  useSourceUrl: () => ({ url: "blob:reviewed-image", error: null }),
}));
let serial = 0;
const source: SourceDraft = {
  key: "S1",
  blobKey: "image",
  artifactId: "artifact",
  sha256: "abc",
  mediaType: "image/png",
  width: 640,
  height: 360,
  durationMs: null,
  origin: { kind: "phone_import", original_name: null },
  capturedAt: 123,
  derivedFrom: null,
  derivation: null,
};
function setup() {
  const profileId = `annotation-proof-${++serial}`;
  const session = new HostSession(profileId, {
    connect() {},
    disconnect() {},
    send: () => false,
    listen: () => () => {},
  });
  session.state.set({
    ...emptyHostState,
    readiness: { checked_at: 0, checks: [] },
  });
  const openExternal = vi
    .fn<(url: string) => Promise<void>>()
    .mockResolvedValue();
  const app: App = {
    session,
    ui: new UiStore(profileId),
    profile: {
      profileId,
      hostId: "host",
      deviceId: "phone",
      name: "Computer",
      addresses: [],
      certSha256: "",
      pairedAt: 0,
    },
    profiles: [],
    platform: { kind: "simulated", openExternal } as unknown as Platform,
    switchTo() {},
    pairAnother() {},
    async rename() {},
    forget: async () => {},
  };
  return { app, openExternal };
}
function fixture(): AnnotationDraft {
  const store = new AnnotationStore(`trim-${++serial}`),
    d = store.create("project", null);
  return {
    ...d,
    sources: [
      {
        ...source,
        key: "clip",
        mediaType: "video/webm",
        included: false,
        durationMs: 10000,
      },
      {
        ...source,
        key: "frame",
        derivedFrom: "clip",
        derivation: { kind: "frame_from_clip", at_ms: 3500 },
        origin: { kind: "clip", duration_ms: 10000, target_id: null },
      },
      {
        ...source,
        key: "excluded",
        derivedFrom: "clip",
        derivation: { kind: "frame_from_clip", at_ms: 1000 },
      },
    ],
    steps: [
      {
        index: 1,
        source_key: "frame",
        text: "Open the menu",
        clip_range_ms: [0, 10000],
      },
    ],
    clipEdits: { clip: { start: "2", end: "6", at: 3.5 } },
  };
}
beforeEach(() => {
  localStorage.clear();
  vi.stubGlobal(
    "ResizeObserver",
    class {
      observe() {}
      disconnect() {}
    },
  );
  vi.stubGlobal("matchMedia", () => ({
    matches: false,
    addEventListener() {},
    removeEventListener() {},
  }));
});
afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("clip selection and outgoing review", () => {
  it.each([
    ["", "3"],
    ["4", "2"],
    ["2", "2"],
    ["-1", "4"],
    ["0", "11"],
    ["NaN", "4"],
  ])("refuses invalid range %s–%s", (start, end) =>
    expect(clipRange(start, end, 10)).toBeNull(),
  );
  it("clamps seeks and excludes out-of-range frames without deleting user work", () => {
    expect(boundedMoment(0, { start: 2, end: 6 })).toBe(2);
    expect(boundedMoment(9, { start: 2, end: 6 })).toBe(6);
    const d = fixture();
    expect(reproductionProblem(d)).toMatch(/outside/);
    const applied = applyClipTrim(d, "clip", { start: 2, end: 6 });
    expect(applied.sources.find((s) => s.key === "excluded")?.included).toBe(
      false,
    );
    expect(applied.sources).toHaveLength(3);
    expect(applied.steps[0]?.clip_range_ms).toEqual([2000, 6000]);
    expect(reproductionProblem(applied)).toBeNull();
  });
  it("reviews the exact images, ordered text, clip time and trim offset", () => {
    render(
      <ReproductionReview
        draft={applyClipTrim(fixture(), "clip", { start: 2, end: 6 })}
      />,
    );
    expect(screen.getAllByRole("img")).toHaveLength(1);
    expect(screen.getByText("Open the menu")).toBeTruthy();
    expect(screen.getByText(/1.50 s into trim/)).toBeTruthy();
    expect(screen.getByText(/Sending these steps does not send a video/)).toBeTruthy();
    expect(screen.getAllByText(/clip at 3.50 s/)).toHaveLength(2);
  });
  it("refuses an out-of-trim bundle before uploading", async () => {
    const store = new AnnotationStore("outside-trim");
    const d = fixture();
    store.set({ [d.id]: d });
    const { app } = setup();
    await expect(
      toBundle(
        {
          platform: app.platform,
          profileId: app.profile.profileId,
          session: app.session,
        },
        store,
        d.id,
      ),
    ).rejects.toThrow(/outside the trim/);
  });
  it("bundles only selected images and records clip provenance", async () => {
    const store = new AnnotationStore("bundle-proof");
    const d = applyClipTrim(fixture(), "clip", { start: 2, end: 6 });
    // This fixture models sources already retained as immutable host artifacts.
    d.sources = d.sources.map(source => ({ ...source, blobKey: null }));
    store.set({ [d.id]: d });
    const { app } = setup();
    const bundle = await toBundle(
      {
        platform: app.platform,
        profileId: app.profile.profileId,
        session: app.session,
      },
      store,
      d.id,
    );
    expect(bundle.sources.map((s) => s.key)).toEqual(["frame"]);
    expect(bundle.sources[0]?.derived_from).toBe("clip");
    expect(bundle.sources[0]?.derivation).toEqual({
      kind: "frame_from_clip",
      at_ms: 3500,
    });
    expect(bundle.steps[0]?.clip_range_ms).toEqual([2000, 6000]);
    const { rerender } = render(
      <ReproductionReceipt bundle={bundle} delivery="host_accepted" />,
    );
    expect(
      screen.getByText(/Sent to the computer: 1 image and 1 step/),
    ).toBeTruthy();
    rerender(
      <ReproductionReceipt bundle={bundle} delivery="native_delivered" />,
    );
    expect(screen.getByText(/Agent received/)).toBeTruthy();
    rerender(<ReproductionReceipt bundle={bundle} delivery="unknown" />);
    expect(screen.getByText(/Delivery unconfirmed/)).toBeTruthy();
  });
});
describe("capture attribution", () => {
  const artifact: ArtifactMeta = {
    artifact_id: "a",
    media_type: "image/png",
    size: 1,
    sha256: "hash",
    created_at: 600,
    label: null,
    task_id: null,
    capture: {
      target_id: "output",
      captured_at: 400,
      geometry: {
        width: 800,
        height: 600,
        scale: 1.5,
        rotation_degrees: 90,
        revision: 9,
      },
    },
  };
  it("uses returned geometry and marks a changed capture", () => {
    const actual = captureProvenance(artifact, "output", 8);
    expect(actual.origin.geometry_revision).toBe(9);
    expect(actual.captureGeometry.captured_at).toBe(400);
    expect(actual.geometryChanged).toBe(true);
    expect(captureProvenance(artifact, "output", 9).geometryChanged).toBe(
      false,
    );
  });
  it("refuses absent or mismatched capture provenance", () => {
    expect(() =>
      captureProvenance({ ...artifact, capture: undefined }, "output", 9),
    ).toThrow(/geometry/);
    expect(() => captureProvenance(artifact, "different", 9)).toThrow(
      /geometry/,
    );
  });
});
describe("annotation composer", () => {
  it("keeps the newest mark selected and in view, and has one draft-preserving back control", () => {
    const { app } = setup(),
      store = annotationStore(app.profile.profileId),
      d = store.create("project", null);
    store.record(d.id, (v) => ({
      ...v,
      sources: [source],
      selectedSource: source.key,
    }));
    const route = { screen: "annotate", annotationId: d.id } as const;
    render(
      <AppProvider app={app}>
        <RouteProvider value={{ tab: "talk", route, depth: 1, active: true }}>
          <Annotate route={route} />
        </RouteProvider>
      </AppProvider>,
    );
    expect(
      screen.getAllByRole("button", { name: "Back and keep draft" }),
    ).toHaveLength(1);
    expect(
      screen.queryByRole("button", { name: "Close and keep draft" }),
    ).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: /^Pin$/ }));
    fireEvent.click(screen.getByRole("button", { name: "Add pin in center" }));
    const field = screen.getByLabelText("What should change?");
    fireEvent.focus(field);
    fireEvent.change(field, { target: { value: "Move this heading" } });
    expect(
      screen.getByRole("img", { name: /^S1: image with 1 mark/ }),
    ).toBeTruthy();
    expect(field.getAttribute("placeholder")).toBe("What should change about A?");
    expect(
      (screen.getByRole("button", { name: "Add comment" }) as HTMLButtonElement)
        .disabled,
    ).toBe(false);
    const review = screen.getByRole("button", { name: "Review and send" });
    expect(review.getAttribute("aria-disabled")).toBe("true");
    fireEvent.click(screen.getByRole("button", { name: "Add comment" }));
    expect(document.activeElement).toBe(field);
    expect(document.querySelector(".is-composing")).toBeTruthy();
    expect(store.get()[d.id]?.comments[0]?.mark_ids).toEqual([
      store.get()[d.id]?.marks[0]?.mark_id,
    ]);
    expect(review.getAttribute("aria-disabled")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Target A" }));
    fireEvent.change(field, { target: { value: "Another note" } });
    expect(screen.getByText(/Choose a mark or General comment/)).toBeTruthy();
  });
  it("keeps the picture above the field while typing and shows whole source pictures", async () => {
    HTMLDialogElement.prototype.showModal = function () { this.open = true; };
    HTMLDialogElement.prototype.close = function () { this.open = false; };
    // The screen's own stylesheet, so the test sees what it shows and hides.
    // Vitest empties CSS imports, so it is read from disk.
    const fs = await vi.importActual<{ readFileSync(path: string, encoding: "utf8"): string }>("node:fs");
    const style = document.createElement("style");
    style.textContent = fs.readFileSync("src/screens/annotate/annotate.css", "utf8");
    document.head.append(style);
    const { app } = setup(),
      store = annotationStore(app.profile.profileId),
      d = store.create("project", null);
    store.record(d.id, (v) => ({
      ...v,
      sources: [source],
      selectedSource: source.key,
    }));
    const route = { screen: "annotate", annotationId: d.id } as const;
    render(
      <AppProvider app={app}>
        <RouteProvider value={{ tab: "talk", route, depth: 1, active: true }}>
          <Annotate route={route} />
        </RouteProvider>
      </AppProvider>,
    );
    const shown = (element: Element | null) => {
      if (!element) throw new Error("Element not rendered");
      for (let e: Element | null = element; e; e = e.parentElement)
        if (getComputedStyle(e).display === "none") return false;
      return true;
    };
    try {
      expect(document.querySelector(".an-source img")).toBeNull();
      fireEvent.click(screen.getByRole("button", { name: /^Draft/ }));
      const thumbnail = document.querySelector(".an-source img");
      expect(thumbnail && getComputedStyle(thumbnail).objectFit).toBe("contain");
      fireEvent.click(screen.getByRole("button", { name: /^Pin$/ }));
      fireEvent.click(screen.getByRole("button", { name: "Add pin in center" }));
      fireEvent.focus(screen.getByLabelText("What should change?"));
      expect(document.querySelector(".is-composing")).toBeTruthy();
      expect(shown(screen.getByRole("img", { name: /^S1: image with 1 mark/ }))).toBe(true);
      expect(shown(document.querySelector(".an-tools"))).toBe(false);
      expect(shown(document.querySelector(".an-targets"))).toBe(true);
    } finally {
      style.remove();
    }
  });
  it("starts again at A after every mark is undone", () => {
    const { app } = setup(),
      store = annotationStore(app.profile.profileId),
      d = store.create("project", null);
    store.record(d.id, (v) => ({
      ...v,
      sources: [source],
      selectedSource: source.key,
    }));
    const route = { screen: "annotate", annotationId: d.id } as const;
    render(
      <AppProvider app={app}>
        <RouteProvider value={{ tab: "talk", route, depth: 1, active: true }}>
          <Annotate route={route} />
        </RouteProvider>
      </AppProvider>,
    );
    const labels = () => store.get()[d.id]?.marks.map((m) => m.label);
    fireEvent.click(screen.getByRole("button", { name: /^Pin$/ }));
    fireEvent.click(screen.getByRole("button", { name: "Add pin in center" }));
    fireEvent.click(screen.getByRole("button", { name: "Add pin in center" }));
    expect(labels()).toEqual(["A", "B"]);
    fireEvent.click(screen.getByRole("button", { name: "Undo" }));
    fireEvent.click(screen.getByRole("button", { name: "Undo" }));
    expect(labels()).toEqual([]);
    fireEvent.click(screen.getByRole("button", { name: "Add pin in center" }));
    expect(labels()).toEqual(["A"]);
  });
});
describe("preview fallback states", () => {
  it("refuses phone-local navigation when private forwarding is unavailable", async () => {
    const { app, openExternal } = setup();
    render(
      <AppProvider app={app}>
        <PreviewCard
          preview={{
            preview_id: "p",
            project_id: "project",
            label: "Development",
            url: "http://localhost:5173",
            checked_at: 100,
            reachable: true,
          }}
        />
      </AppProvider>,
    );
    expect(
      screen.getByText(/Private previews are unavailable on this device/),
    ).toBeTruthy();
    expect(screen.getByText(/Reachable from the computer/)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Open Development" }));
    expect(openExternal).not.toHaveBeenCalled();
  });
  it("keeps opening pending until the browser responds and reports an error", async () => {
    const { app, openExternal } = setup();
    app.session.link.set({ state: "online", since: Date.now() });
    let rejectOpen: (reason: Error) => void = () => {};
    openExternal.mockImplementation(
      () =>
        new Promise<void>((_, reject) => {
          rejectOpen = reject;
        }),
    );
    render(
      <AppProvider app={app}>
        <PreviewCard
          preview={{
            preview_id: "p",
            project_id: "project",
            label: "Development",
            url: "http://192.168.0.4:5173",
            checked_at: null,
            reachable: null,
          }}
        />
      </AppProvider>,
    );
    fireEvent.click(screen.getByRole("button", { name: "Open Development" }));
    expect(
      screen.getByRole("button", { name: /Opening preview/ }),
    ).toBeTruthy();
    await act(async () => {
      rejectOpen(new Error("Browser unavailable"));
    });
    expect(screen.getByText("Browser unavailable")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Open Development" })).toBeTruthy();
  });
  it("disables opening after access is revoked", () => {
    const { app, openExternal } = setup();
    app.session.link.set({
      state: "failed",
      reason: "revoked",
      detail: "Revoked by owner",
    });
    render(
      <AppProvider app={app}>
        <PreviewCard
          preview={{
            preview_id: "p",
            project_id: "project",
            label: "Development",
            url: "http://localhost:5173",
            checked_at: null,
            reachable: false,
          }}
        />
      </AppProvider>,
    );
    const button = screen.getByRole("button", {
      name: "Open Development",
    }) as HTMLButtonElement;
    expect(button.disabled).toBe(true);
    fireEvent.click(button);
    expect(openExternal).not.toHaveBeenCalled();
    expect(screen.getByText("This device’s access was revoked.")).toBeTruthy();
  });
  it("retains direct LAN opens and rejects non-web addresses", () => {
    expect(previewAddress("http://192.168.0.4:5173").loopback).toBe(false);
    expect(previewAddress("http://[::1]:5173").loopback).toBe(true);
    expect(() => previewAddress("javascript:alert(1)")).toThrow();
  });
});

it("carries preview, computer and capture time from annotation into the sent receipt", async () => {
  const { app } = setup(), store = annotationStore(app.profile.profileId), d = store.create("project", null);
  const preview: SourceDraft = { ...source, blobKey: null, capturedAt: 1790582400000,
    origin: { kind: "preview_capture", capture_id: "capture", host_id: "host", computer_name: "studio",
      preview_id: "notes-preview", preview_label: "Notes", captured_at: 1790582400000 } };
  store.record(d.id, v => ({ ...v, sources: [preview], selectedSource: source.key }));
  const route = { screen: "annotate", annotationId: d.id } as const;
  const view = render(<AppProvider app={app}><RouteProvider value={{ tab: "talk", route, depth: 1, active: true }}>
    <Annotate route={route} />
  </RouteProvider></AppProvider>);
  expect(screen.getAllByText(/Taken from the Notes preview on studio/)[0]!.textContent).toContain(new Date(preview.capturedAt).toLocaleString());
  expect(screen.queryByText(/original capture time and computer are unknown/)).toBeNull();
  const bundle = await toBundle({ platform: app.platform, profileId: app.profile.profileId, session: app.session }, store, d.id);
  expect(bundle.sources[0]).toMatchObject({ origin: preview.origin, captured_at: preview.capturedAt });
  view.unmount();
  render(<ReproductionReceipt bundle={bundle} delivery="host_accepted" />);
  expect(screen.getAllByText(/Taken from the Notes preview on studio/)[0]!.textContent).toContain(new Date(preview.capturedAt).toLocaleString());
});

it("keeps the preview open after capture and offers an explicit annotation route", async () => {
  const { previewDrafts } = await import("../control/previewDrafts");
  const { app } = setup();
  vi.spyOn(previewDrafts, "list").mockResolvedValue([]);
  const put = vi.spyOn(previewDrafts, "put").mockResolvedValue();
  vi.spyOn(URL, "createObjectURL").mockReturnValue("blob:preview");
  vi.spyOn(URL, "revokeObjectURL").mockImplementation(() => {});
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  app.session.link.set({ state: "online", since: Date.now() });
  app.session.state.set(s => ({ ...s, grants: { ...s.grants!, tasks: true } }));
  vi.spyOn(app.session, "command").mockResolvedValue({ command_id: "preview-check", status: "accepted", seq: 1,
    error: null, result: { kind: "preview_registered", preview_id: "p" } });
  const close = vi.fn(async () => {}), acknowledge = vi.fn(async () => {});
  const picture = vi.fn(async () => ({ id: "picture", blob: new Blob(["png"], { type: "image/png" }), acknowledge }));
  app.platform.preview = vi.fn(async () => ({ document: "<h1>Notes</h1>", picture, close, show: async () => null }));
  await act(async () => { render(<AppProvider app={app}><RouteProvider value={{ tab: "control", route: { screen: "control" }, depth: 0, active: true }}><PreviewCard preview={{ preview_id: "p", project_id: "project", label: "Notes", url: "http://localhost:3000", reachable: true, checked_at: 1 }} /></RouteProvider></AppProvider>); });
  await act(async () => fireEvent.click(screen.getByRole("button", { name: "Open Notes" })));
  await act(async () => fireEvent.click(screen.getByRole("button", { name: "Take picture" })));
  expect(screen.getByRole("dialog", { name: "Notes" })).toBeTruthy();
  expect(screen.getByText("Picture saved. Take another or close the preview.")).toBeTruthy();
  expect(screen.getAllByRole("button", { name: "Annotate picture" }).some(b => b.closest("dialog[open]"))).toBe(true);
  expect(put).toHaveBeenCalledWith(expect.objectContaining({ previewLabel: "Notes", computerName: "Computer" }));
  expect(acknowledge).toHaveBeenCalledOnce();
  expect(close).not.toHaveBeenCalled();
});
