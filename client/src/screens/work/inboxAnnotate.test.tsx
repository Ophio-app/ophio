// SPDX-License-Identifier: Apache-2.0
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AppProvider, type App } from "../../app/context";
import { UiStore } from "../../app/ui";
import { HostSession } from "../../host/session";
import type { Platform } from "../../host/link";
import { upload } from "../../host/transfer";
import { emptyHostState, type HostState } from "../../state/projection";
import { annotationStore, blobs, toBundle } from "../../state/annotations";
import { RouteProvider } from "../../shell/Screen";
import { verifiedSource } from "../annotate/derivatives";
import Inbox from "./Inbox";
import { inboxStore } from "./inbox";

vi.mock("./pictures", async (original) => ({
  ...(await original<typeof import("./pictures")>()),
  // jsdom never decodes images.
  imageSize: vi.fn(async () => ({ width: 40, height: 30 })),
}));
vi.mock("../../host/transfer", async (original) => ({
  ...(await original<typeof import("../../host/transfer")>()),
  upload: vi.fn(),
}));

const bytes = new TextEncoder().encode("shared screenshot bytes");
const picture = () =>
  Object.assign(new Blob([bytes], { type: "image/png" }), {
    arrayBuffer: async () => bytes.slice().buffer,
  });

let serial = 0;
function setup() {
  const profileId = `inbox-annotate-${++serial}`;
  const session = new HostSession(profileId, {
    connect() {},
    disconnect() {},
    send: () => false,
    listen: () => () => {},
  });
  session.state.set({
    ...emptyHostState,
    projects: {
      garden: { project_id: "garden", name: "Garden", host_path: "/work/garden" },
    },
  } as unknown as HostState);
  const ui = new UiStore(profileId);
  const app: App = {
    session,
    ui,
    profile: { profileId, hostId: "host", deviceId: "phone", name: "Computer", addresses: [], certSha256: "", pairedAt: 0 },
    profiles: [],
    platform: { kind: "simulated" } as unknown as Platform,
    switchTo() {},
    pairAnother() {},
    async rename() {},
    forget: async () => {},
  };
  return { app, ui, profileId };
}

beforeEach(() => {
  localStorage.clear();
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
  vi.spyOn(blobs, "remove").mockImplementation(async (key) => { stored.delete(key); });
});
afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

it.each(["Annotate", "Use in a message"])(
  "a shared image taken into a draft via “%s” can be shown and sent",
  async (action) => {
    const { app, ui, profileId } = setup();
    const inbox = inboxStore(profileId);
    await inbox.addBlob(picture(), { kind: "image", name: "Screenshot.png", sourceApp: null, projectId: null });
    render(
      <AppProvider app={app}>
        <RouteProvider value={{ tab: "work", route: { screen: "inbox" }, depth: 1, active: true }}>
          <Inbox route={{ screen: "inbox" }} />
        </RouteProvider>
      </AppProvider>,
    );
    fireEvent.click(screen.getByRole("button", { name: action }));
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: /Garden/ }));
    });
    const store = annotationStore(profileId);
    await waitFor(() => expect(Object.values(store.get())).toHaveLength(1));
    const draft = Object.values(store.get())[0]!;
    const source = draft.sources[0]!;
    // The annotate canvas renders only what verifiedSource accepts.
    await expect(verifiedSource(source)).resolves.toBeTruthy();
    vi.mocked(upload).mockResolvedValue({ transferId: "t", artifactId: "artifact", sha256: source.sha256 ?? "" });
    const bundle = await toBundle({ platform: app.platform, profileId, session: app.session }, store, draft.id);
    expect(bundle.sources).toHaveLength(1);
    expect(upload).toHaveBeenCalledOnce();
    ui.persistNow();
  },
);
