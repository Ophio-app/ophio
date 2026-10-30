// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { HostSession } from "../../host/session";
import type { Platform } from "../../host/link";
import { emptyHostState, type HostState } from "../../state/projection";
import { annotationStore, blobs } from "../../state/annotations";
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

function setup() {
  const profileId = "inbox-annotate-back";
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
const inboxView = (app: App) =>
  render(
    <AppProvider app={app}>
      <RouteProvider value={{ tab: "work", route: { screen: "inbox" }, depth: 1, active: true }}>
        <Inbox route={{ screen: "inbox" }} />
      </RouteProvider>
    </AppProvider>,
  );
async function annotateInGarden(ui: UiStore) {
  fireEvent.click(screen.getByRole("button", { name: "Annotate" }));
  await act(async () => {
    fireEvent.click(screen.getByRole("button", { name: /Garden/ }));
  });
  await waitFor(() => expect(ui.route()).toMatchObject({ screen: "annotate" }));
  return ui.route() as { screen: "annotate"; annotationId: string };
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

it("marks made on a shared image survive “Back and keep draft”", async () => {
  const { app, ui, profileId } = setup();
  await inboxStore(profileId).addBlob(picture(), { kind: "image", name: "Shared.png", sourceApp: null, projectId: null });
  const inbox = inboxView(app);
  const route = await annotateInGarden(ui);
  inbox.unmount();

  const annotate = render(
    <AppProvider app={app}>
      <RouteProvider value={{ tab: "work", route, depth: 2, active: true }}>
        <Annotate route={route} />
      </RouteProvider>
    </AppProvider>,
  );
  fireEvent.click(screen.getByRole("button", { name: /^Pin$/ }));
  fireEvent.click(screen.getByRole("button", { name: "Add pin in center" }));
  expect(annotationStore(profileId).get()[route.annotationId]?.marks).toHaveLength(1);
  // "Back and keep draft".
  annotate.unmount();
  ui.back();

  // The kept draft is reachable from Garden's Talk draft (Attach, then Mark up)…
  const kept = annotationStore(profileId).get()[ui.draft("garden", null)?.annotationId ?? ""];
  expect(kept?.marks.map((m) => m.label)).toEqual(["A"]);
  // …and annotating the same shared item again returns to those marks.
  inboxView(app);
  const again = await annotateInGarden(ui);
  expect(annotationStore(profileId).get()[again.annotationId]?.marks.map((m) => m.label)).toEqual(["A"]);
  ui.persistNow();
});
