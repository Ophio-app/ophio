// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { HostSession } from "../../host/session";
import type { Platform } from "../../host/link";
import { emptyHostState, type HostState } from "../../state/projection";
import { annotationStore, blobs, type SourceDraft } from "../../state/annotations";
import { RouteProvider } from "../../shell/Screen";
import Annotate from "../annotate/Annotate";
import Inbox from "./Inbox";
import { inboxStore } from "./inbox";

vi.mock("./pictures", async (original) => ({
  ...(await original<typeof import("./pictures")>()),
  imageSize: vi.fn(async () => ({ width: 40, height: 30 })),
}));
vi.mock("../annotate/useSourceUrl", () => ({
  useSourceUrl: () => ({ url: "blob:picture", error: null }),
}));

const bytes = new TextEncoder().encode("shared screenshot bytes");
const picture = () =>
  Object.assign(new Blob([bytes], { type: "image/png" }), {
    arrayBuffer: async () => bytes.slice().buffer,
  });
const earlier: SourceDraft = {
  key: "S1", blobKey: "earlier-bytes", artifactId: "earlier-artifact", sha256: "a".repeat(64),
  mediaType: "image/png", width: 100, height: 80, durationMs: null,
  origin: { kind: "phone_import", original_name: "Earlier.png" }, capturedAt: 1,
  derivedFrom: null, derivation: null,
};

let serial = 0;
function setup() {
  const profileId = `inbox-keeps-markup-${++serial}`;
  const session = new HostSession(profileId, { connect() {}, disconnect() {}, send: () => false, listen: () => () => {} });
  session.state.set({
    ...emptyHostState,
    readiness: { checked_at: 0, checks: [] },
    projects: { garden: { project_id: "garden", name: "Garden", host_path: "/work/garden" } },
  } as unknown as HostState);
  const ui = new UiStore(profileId);
  const app: App = {
    session, ui,
    profile: { profileId, hostId: "host", deviceId: "phone", name: "Computer", addresses: [], certSha256: "", pairedAt: 0 },
    profiles: [], platform: { kind: "simulated" } as unknown as Platform,
    switchTo() {}, pairAnother() {}, async rename() {}, forget: async () => {},
  };
  return { app, ui, profileId };
}

beforeEach(() => {
  localStorage.clear();
  vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
  vi.stubGlobal("matchMedia", () => ({ matches: false, addEventListener() {}, removeEventListener() {} }));
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  const stored = new Map<string, Blob>();
  let next = 0;
  vi.spyOn(blobs, "put").mockImplementation(async (blob) => {
    const key = `blob-${++next}`;
    stored.set(key, blob);
    return key;
  });
  vi.spyOn(blobs, "get").mockImplementation(async (key) => stored.get(key) ?? null);
});
afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

it("annotating a shared image keeps the markup already attached to the project’s draft", async () => {
  const { app, ui, profileId } = setup();
  // The person already marked up a screenshot for a new Garden task.
  const annotations = annotationStore(profileId);
  const markup = annotations.create("garden", null);
  annotations.record(markup.id, (d) => ({ ...d, sources: [earlier], nextSource: 2 }));
  ui.saveDraft("garden", null, { text: "Fix the header", annotationId: markup.id });

  await inboxStore(profileId).addBlob(picture(), { kind: "image", name: "Shared.png", sourceApp: null, projectId: null });
  const inbox = render(
    <AppProvider app={app}>
      <RouteProvider value={{ tab: "work", route: { screen: "inbox" }, depth: 1, active: true }}>
        <Inbox route={{ screen: "inbox" }} />
      </RouteProvider>
    </AppProvider>,
  );
  fireEvent.click(screen.getByRole("button", { name: "Annotate" }));
  await act(async () => {
    fireEvent.click(screen.getByRole("button", { name: /Garden/ }));
  });
  await waitFor(() => expect(ui.route()).toMatchObject({ screen: "annotate" }));
  const route = ui.route() as { screen: "annotate"; annotationId: string };
  inbox.unmount();

  // The annotate screen opens on the shared image, then the person reviews.
  render(
    <AppProvider app={app}>
      <RouteProvider value={{ tab: "work", route, depth: 2, active: true }}>
        <Annotate route={route} />
      </RouteProvider>
    </AppProvider>,
  );
  expect(screen.getAllByText(/Shared\.png|From this device/).length).toBeGreaterThan(0);
  fireEvent.click(screen.getByRole("button", { name: "Review and send" }));

  const attached = annotations.get()[ui.draft("garden", null)?.annotationId ?? ""];
  // The earlier marked-up screenshot is still part of the message under review.
  expect(attached?.sources.map((s) => s.origin)).toEqual([
    earlier.origin,
    { kind: "phone_import", original_name: "Shared.png" },
  ]);
  // …and the annotate screen showed the newly shared image, not the earlier one.
  expect(attached?.selectedSource).toBe("S2");
  ui.persistNow();
});
